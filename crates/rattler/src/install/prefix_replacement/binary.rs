//! Prefix replacement in binary files, where a replacement must preserve the file length: the
//! remainder of the affected c-string shifts towards the start and the gap before its NUL
//! terminator is filled with zeros.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::sync::{LazyLock, Mutex};

use aho_corasick::{AhoCorasick, AhoCorasickKind};

use rattler_conda_types::package::{
    InvalidOffsetsError, OffsetEncoding, OffsetGroup, OffsetRanges,
};

use super::{
    CStringOccurrences, CStringPatch, EncodedPrefix, InconsistentOffsetsError, OffsetReplaceError,
    encoded_prefix_for, reject_growing_prefix, write_replacement_range,
};

/// Given the contents of a file, copies it to the `destination` and in the process replace any
/// binary c-style string that contains the text `prefix_placeholder` with a binary compatible
/// c-string where the `prefix_placeholder` text is replaced with the `target_prefix` text.
///
/// The length of the input will match the output.
///
/// The placeholder is replaced under every encoding defined by the draft CEP, so paths stored as
/// wide strings are patched as well. The NUL terminator of a c-string is the zero code unit of its
/// encoding: one zero byte for UTF-8, two for UTF-16 and four for UTF-32.
///
/// This function replaces binary c-style strings. If you want to simply find-and-replace text in a
/// file instead use the [`super::copy_and_replace_textual_placeholder`] function.
pub fn copy_and_replace_cstring_placeholder(
    source_bytes: &[u8],
    destination: impl Write,
    prefix_placeholder: &str,
    target_prefix: &str,
) -> Result<(), std::io::Error> {
    let prefixes = EncodedPrefix::all(prefix_placeholder, target_prefix);

    let patches = find_cstring_patches(source_bytes, &prefixes);
    // Only the encodings that actually occur in the file constrain the target prefix: an encoding
    // whose replacement would not fit but that the file does not use is irrelevant.
    reject_growing_prefix(patches.iter().map(|patch| patch.prefix))?;

    write_patched_cstrings(destination, source_bytes, &patches)
}

/// Finds the c-strings that search-based binary replacement patches, under every encoding the
/// draft CEP defines and ordered by position in the file.
///
/// This exposes the search [`copy_and_replace_cstring_placeholder`] performs, including how it
/// resolves overlapping candidates of different encodings, so that callers patching files lazily,
/// such as the mount-time ranged reads in `rattler_vfs`, stay byte-identical with it.
pub fn find_cstring_occurrences(
    source_bytes: &[u8],
    prefix_placeholder: &str,
) -> Vec<CStringOccurrences> {
    // Only the placeholder is searched for, so the target prefix does not matter here.
    let prefixes = EncodedPrefix::all(prefix_placeholder, "");
    find_cstring_patches(source_bytes, &prefixes)
        .into_iter()
        .map(|patch| CStringOccurrences {
            offsets: patch.offsets.into_owned(),
            nul_pos: patch.nul_pos,
            encoding: patch.prefix.encoding,
        })
        .collect()
}

/// Finds every c-string that contains a placeholder occurrence, under every encoding, ordered by
/// position in the file.
///
/// Candidates of different encodings can describe overlapping c-strings, because the encodings of
/// an ASCII placeholder are byte-shifted variants of one another: UTF-16-BE of `/pfx` is UTF-16-LE
/// of `/pfx` shifted by one byte, so a genuine little-endian occurrence preceded by a zero byte
/// always produces a spurious big-endian candidate one byte earlier (and the other way around).
/// A spurious candidate is misaligned with the string it sits in, so its terminator scan often
/// finds no zero code unit at all and its c-string would swallow the rest of the file.
///
/// Overlaps are therefore resolved by preferring, in order: UTF-8 (whose encoding cannot be a
/// shifted variant of another one), a candidate whose offset is code-unit aligned in the file
/// (which a compiler-emitted wide string is and its shifted twin is not), a c-string with a real
/// terminator over one that runs to end-of-file, and finally the earlier offset. A candidate that
/// loses leaves its placeholder in place, which is what rattler did before it replaced wide
/// strings at all; writing another encoding's bytes over the string instead would corrupt it.
///
/// A candidate collects every occurrence up to its terminator, so a later occurrence at the same
/// alignment inside that range is skipped instead of starting a candidate of its own: it would
/// lose to the earlier one anyway, and building it rescans the c-string, which is quadratic for
/// long c-strings with many occurrences.
fn find_cstring_patches<'a>(
    source_bytes: &[u8],
    prefixes: &'a [EncodedPrefix],
) -> Vec<CStringPatch<'a>> {
    let mut candidates = Vec::new();
    for (prefix, occurrences) in prefixes
        .iter()
        .zip(find_occurrences(source_bytes, prefixes))
    {
        let placeholder_len = prefix.placeholder.len();
        let unit = prefix.code_unit_size();
        // Per alignment class (offset modulo the code unit size), the end of the last candidate.
        let mut covered_until = vec![0; unit];
        // Every occurrence is visited, not just the first one after a c-string: an occurrence of
        // the same encoding at a different alignment can start before this terminator.
        for (i, &first) in occurrences.iter().enumerate() {
            let after_first = first + placeholder_len;
            let alignment = first % unit;
            if first < covered_until[alignment] {
                continue;
            }

            let nul_pos = cstring_end(source_bytes, after_first, unit);

            // Collect the remaining occurrences in the same c-string: they share its terminator
            // and the padding that keeps the file length unchanged.
            let end = nul_pos.unwrap_or(source_bytes.len());
            let offsets: Vec<usize> = occurrences[i..]
                .iter()
                .copied()
                .take_while(|&offset| offset + placeholder_len <= end)
                .collect();
            covered_until[alignment] = end;

            let rank = match (&prefix.encoding, alignment == 0, nul_pos.is_some()) {
                (OffsetEncoding::Utf8, _, _) => 0u8,
                (_, true, true) => 1,
                (_, true, false) => 2,
                (_, false, true) => 3,
                (_, false, false) => 4,
            };
            candidates.push((
                rank,
                CStringPatch {
                    offsets: Cow::Owned(offsets),
                    nul_pos: end,
                    prefix,
                },
            ));
        }
    }

    // Keep the best candidates whose c-strings do not overlap a better one. The kept c-strings
    // are disjoint, so only the nearest kept c-string on either side can overlap a candidate.
    candidates.sort_by_key(|(rank, patch)| (*rank, patch.offsets[0]));
    let mut kept: BTreeMap<usize, CStringPatch<'a>> = BTreeMap::new();
    for (_, candidate) in candidates {
        let start = candidate.offsets[0];
        let overlaps_earlier = kept
            .range(..=start)
            .next_back()
            .is_some_and(|(_, earlier)| start < earlier.nul_pos);
        let overlaps_later = kept
            .range(start..)
            .next()
            .is_some_and(|(&later_start, _)| later_start < candidate.nul_pos);
        if !overlaps_earlier && !overlaps_later {
            kept.insert(start, candidate);
        }
    }
    kept.into_values().collect()
}

/// Finds, for each of `prefixes`, the leftmost non-overlapping occurrences of its placeholder in
/// `source_bytes`, in ascending order: exactly what repeated [`memchr::memmem::find`] calls that
/// resume after each match return.
///
/// Binaries are large and rarely contain a placeholder, so this is where search-based replacement
/// spends its time. Searching once per encoding reads the file five times. When every character of
/// the placeholder is below U+0100, though, each of its wide code units is that character's byte
/// padded with zeros, so both byte orders of an encoding share one needle: UTF-16-LE is
/// `c0 00 c1 .. 00 cn 00` and UTF-16-BE is `00 c0 00 c1 .. 00 cn`, both around the core
/// `c0 00 c1 .. 00 cn`, and likewise for UTF-32 with three zero bytes. A single multi-pattern
/// search for the UTF-8 placeholder and the two cores finds every occurrence under every encoding;
/// the zero bytes around a core then tell which byte order (or both) it is.
fn find_occurrences(source_bytes: &[u8], prefixes: &[EncodedPrefix]) -> Vec<Vec<usize>> {
    find_occurrences_single_pass(source_bytes, prefixes).unwrap_or_else(|| {
        prefixes
            .iter()
            .map(|prefix| memchr::memmem::find_iter(source_bytes, &prefix.placeholder).collect())
            .collect()
    })
}

/// The single-pass search of [`find_occurrences`], or `None` when the placeholder does not allow it
/// (a character at or above U+0100, or a single character, whose needles would coincide).
fn find_occurrences_single_pass(
    source_bytes: &[u8],
    prefixes: &[EncodedPrefix],
) -> Option<Vec<Vec<usize>>> {
    let utf8 = prefixes
        .iter()
        .find(|prefix| prefix.encoding == OffsetEncoding::Utf8)?;
    let searcher = core_searcher(&utf8.placeholder)?;

    let zeros = |range: std::ops::Range<usize>| {
        source_bytes
            .get(range)
            .is_some_and(|bytes| bytes.iter().all(|&byte| byte == 0))
    };
    // All occurrences, overlapping ones included, per encoding.
    let mut all: [Vec<usize>; 5] = Default::default();
    let index = |encoding: OffsetEncoding| match encoding {
        OffsetEncoding::Utf8 => 0,
        OffsetEncoding::Utf16Le => 1,
        OffsetEncoding::Utf16Be => 2,
        OffsetEncoding::Utf32Le => 3,
        OffsetEncoding::Utf32Be => 4,
    };
    for found in searcher.find_overlapping_iter(source_bytes) {
        let (start, end) = (found.start(), found.end());
        match found.pattern().as_usize() {
            0 => all[0].push(start),
            pattern => {
                let (pad, le, be) = if pattern == 1 { (1, 1, 2) } else { (3, 3, 4) };
                if zeros(end..end + pad) {
                    all[le].push(start);
                }
                if start >= pad && zeros(start - pad..start) {
                    all[be].push(start - pad);
                }
            }
        }
    }

    Some(
        prefixes
            .iter()
            .map(|prefix| {
                let mut occurrences = std::mem::take(&mut all[index(prefix.encoding)]);
                occurrences.sort_unstable();
                // Keep the leftmost non-overlapping ones, as a search resuming after each match.
                let len = prefix.placeholder.len();
                let mut next_allowed = 0;
                occurrences.retain(|&offset| {
                    let keep = offset >= next_allowed;
                    if keep {
                        next_allowed = offset + len;
                    }
                    keep
                });
                occurrences
            })
            .collect(),
    )
}

/// The searcher for the UTF-8 placeholder and its UTF-16 and UTF-32 cores (pattern 0, 1 and 2), or
/// `None` when the placeholder does not allow the single-pass search.
///
/// Building the automaton costs more than searching a typical binary with it, and a package uses
/// one placeholder for all of its files, so searchers are cached per placeholder.
fn core_searcher(placeholder: &[u8]) -> Option<AhoCorasick> {
    /// More distinct placeholders than this in one process is unusual; the cache is simply reset.
    const MAX_CACHED: usize = 16;
    static CACHE: LazyLock<Mutex<HashMap<Vec<u8>, Option<AhoCorasick>>>> =
        LazyLock::new(Mutex::default);

    if let Some(cached) = CACHE.lock().unwrap().get(placeholder) {
        return cached.clone();
    }
    let searcher = build_core_searcher(placeholder);
    let mut cache = CACHE.lock().unwrap();
    if cache.len() >= MAX_CACHED {
        cache.clear();
    }
    cache.insert(placeholder.to_vec(), searcher.clone());
    searcher
}

fn build_core_searcher(placeholder: &[u8]) -> Option<AhoCorasick> {
    let text = std::str::from_utf8(placeholder).ok()?;
    let chars: Vec<u8> = text
        .chars()
        .map(|c| u8::try_from(u32::from(c)).ok())
        .collect::<Option<_>>()?;
    if chars.len() < 2 {
        return None;
    }
    let core = |unit: usize| -> Vec<u8> {
        let mut core = Vec::with_capacity(chars.len() * unit);
        for &c in &chars {
            core.push(c);
            core.extend(std::iter::repeat_n(0, unit - 1));
        }
        core.truncate(core.len() - (unit - 1));
        core
    };
    AhoCorasick::builder()
        .kind(Some(AhoCorasickKind::DFA))
        .build([placeholder.to_vec(), core(2), core(4)])
        .ok()
}

/// Finds the end of the c-string containing a placeholder occurrence: the offset of the first zero
/// code unit at or after `from`, or `None` when the c-string is unterminated at end-of-file.
///
/// `from` is the end of an occurrence, so the scan strides in code units from there. For a genuine
/// occurrence that is the alignment of the string it sits in; for a spurious cross-encoding match
/// it is not, which is exactly why a missing terminator has to be distinguishable from a found
/// one.
fn cstring_end(source_bytes: &[u8], from: usize, unit: usize) -> Option<usize> {
    if unit == 1 {
        return memchr::memchr(0, source_bytes.get(from..)?).map(|nul| from + nul);
    }
    let mut pos = from;
    while pos + unit <= source_bytes.len() {
        if source_bytes[pos..pos + unit].iter().all(|&byte| byte == 0) {
            return Some(pos);
        }
        pos += unit;
    }
    None
}

/// Writes `source_bytes` to `destination`, replacing the placeholder inside every patched
/// c-string. The bytes that follow a replacement shift towards the start of the string and the gap
/// before its NUL terminator is filled with zeros, so the file length is preserved.
fn write_patched_cstrings(
    mut destination: impl Write,
    source_bytes: &[u8],
    patches: &[CStringPatch<'_>],
) -> Result<(), std::io::Error> {
    let mut last_pos = 0;
    for patch in patches {
        let prefix = patch.prefix;
        for &offset in patch.offsets.iter() {
            write_replacement_range(&mut destination, source_bytes, last_pos, offset)?;
            destination.write_all(&prefix.target)?;
            last_pos = offset + prefix.placeholder.len();
        }

        // Write the remaining bytes of the c-string, which for an unterminated final c-string runs
        // to the end of the file, and fill the gap the replacements left with zeros.
        write_replacement_range(&mut destination, source_bytes, last_pos, patch.nul_pos)?;
        let Some(shrinks_by) = prefix.shrinks_by() else {
            return Err(prefix.growing_prefix_error());
        };
        let padding = patch.offsets.len() * shrinks_by;
        if padding > 0 {
            destination.write_all(&vec![0; padding])?;
        }

        last_pos = patch.nul_pos;
    }

    // Write any remaining bytes after the last c-string.
    if last_pos < source_bytes.len() {
        destination.write_all(&source_bytes[last_pos..])?;
    }

    Ok(())
}

/// Given the contents & offsets of a file, copies it to the `destination` and in the process
/// replace any binary c-style string that contains the text `prefix_placeholder` with a binary
/// compatible c-string where the `prefix_placeholder` text is replaced with the `target_prefix`
/// text.
///
/// The length of the input will match the output.
///
/// Every group of `groups` is applied, each under its own encoding. A group's ranges are grouped
/// by c-string: the inner lists hold the prefix start positions followed by the position of the
/// first byte of the NUL terminator, or the file size when the final c-string is unterminated at
/// end-of-file (the padding then runs to EOF, still preserving the length). For example,
/// `[[5, 19], [22, 30, 39]]` means one c-string with the prefix at offset 5 (NUL at 19), and
/// another with prefixes at 22 and 30 (NUL at 39).
///
/// The metadata is validated before anything is written, so a mismatch surfaces as
/// [`OffsetReplaceError::InconsistentMetadata`] with an untouched destination the caller can hand
/// to search-based replacement.
pub fn copy_and_replace_cstring_placeholder_offsets(
    source_bytes: &[u8],
    destination: impl Write,
    prefix_placeholder: &str,
    target_prefix: &str,
    groups: &[OffsetGroup],
) -> Result<(), OffsetReplaceError> {
    let prefixes = EncodedPrefix::all(prefix_placeholder, target_prefix);

    let patches = cstring_patches_from_groups(groups, &prefixes)?;
    validate_cstring_patches(source_bytes, &patches)?;
    // Only the encodings whose recorded occurrences are present in the file constrain the target
    // prefix. Checking this after the validation keeps bogus metadata a fallback rather than an
    // install failure.
    reject_growing_prefix(patches.iter().map(|patch| patch.prefix))?;

    // --- The metadata is consistent; write the patched file. ---
    write_patched_cstrings(destination, source_bytes, &patches)?;
    Ok(())
}

/// Collects the c-strings to patch from the recorded offset groups, ordered by their position in
/// the file so that the splice runs in file order regardless of which group a range came from.
fn cstring_patches_from_groups<'a>(
    groups: &'a [OffsetGroup],
    prefixes: &'a [EncodedPrefix],
) -> Result<Vec<CStringPatch<'a>>, InconsistentOffsetsError> {
    let mut patches = Vec::new();
    for group in groups {
        let encoding = group.encoding();
        let prefix = encoded_prefix_for(prefixes, encoding)?;
        let OffsetRanges::Binary(cstrings) = group.ranges() else {
            return Err(InvalidOffsetsError::RangesShapeMismatch(encoding).into());
        };
        for cstring in cstrings {
            // Each c-string lists its prefix offsets followed by the NUL terminator position.
            let Some((&nul_pos, offsets)) = cstring.split_last() else {
                return Err(InvalidOffsetsError::ShortCStringRanges(encoding).into());
            };
            if offsets.is_empty() {
                return Err(InvalidOffsetsError::ShortCStringRanges(encoding).into());
            }
            patches.push(CStringPatch {
                offsets: Cow::Borrowed(offsets),
                nul_pos,
                prefix,
            });
        }
    }

    // The binary form must list at least one c-string.
    if patches.is_empty() {
        return Err(InvalidOffsetsError::EmptyList.into());
    }

    patches.sort_by_key(|patch| patch.offsets[0]);
    Ok(patches)
}

/// Validates the c-strings to patch against the file contents.
///
/// Nothing may be written before this passes so that, on inconsistent metadata, the caller can
/// fall back to search-based replacement using the still-empty destination. Within each c-string
/// the prefix offsets must be in range (before the terminator), sorted in strictly increasing
/// non-overlapping order (also across c-strings and encodings), and the placeholder bytes must be
/// present at each offset. The recorded terminator must be a zero code unit of the group's
/// encoding (or end-of-file) at a code-unit distance from every occurrence it terminates:
/// otherwise the padding would be written into the middle of a live string and truncate it.
fn validate_cstring_patches(
    source_bytes: &[u8],
    patches: &[CStringPatch<'_>],
) -> Result<(), InconsistentOffsetsError> {
    let mut prev_end = 0usize;
    for patch in patches {
        let terminator = patch.nul_pos;
        let placeholder = patch.prefix.placeholder.as_slice();
        let unit = patch.prefix.code_unit_size();
        let Some(terminator_bytes) = source_bytes.get(terminator..) else {
            return Err(InconsistentOffsetsError::TerminatorOutOfRange {
                terminator,
                file_size: source_bytes.len(),
            });
        };
        if !terminator_bytes.is_empty()
            && !terminator_bytes
                .get(..unit)
                .is_some_and(|unit| unit.iter().all(|&byte| byte == 0))
        {
            return Err(InconsistentOffsetsError::TerminatorNotZero { terminator });
        }
        for &offset in patch.offsets.iter() {
            if offset < prev_end {
                return Err(InconsistentOffsetsError::UnsortedOffset { offset });
            }
            let end = offset
                .checked_add(placeholder.len())
                .filter(|&end| end <= terminator)
                .ok_or(InconsistentOffsetsError::OffsetPastTerminator { offset, terminator })?;
            if (terminator - offset) % unit != 0 {
                return Err(InconsistentOffsetsError::MisalignedOffset { offset, terminator });
            }
            if &source_bytes[offset..end] != placeholder {
                return Err(InconsistentOffsetsError::PlaceholderNotFound { offset });
            }
            prev_end = end;
        }
        prev_end = terminator;
    }
    Ok(())
}

#[cfg(test)]
mod search_tests {
    use proptest::prelude::*;

    use super::*;

    /// The search as it was before the single pass: one scan per encoding, resumed after every
    /// match, and the remaining occurrences of a c-string collected by scanning it again.
    fn reference_patches(
        source_bytes: &[u8],
        prefixes: &[EncodedPrefix],
    ) -> Vec<(usize, usize, Vec<usize>, OffsetEncoding)> {
        let mut candidates = Vec::new();
        for prefix in prefixes {
            let placeholder = prefix.placeholder.as_slice();
            let unit = prefix.code_unit_size();
            let mut covered_until = vec![0; unit];
            let mut search_from = 0;
            while let Some(found) = memchr::memmem::find(&source_bytes[search_from..], placeholder)
            {
                let first = search_from + found;
                let after_first = first + placeholder.len();
                search_from = after_first;
                let alignment = first % unit;
                if first < covered_until[alignment] {
                    continue;
                }
                let mut pos = after_first;
                let mut nul_pos = None;
                while pos + unit <= source_bytes.len() {
                    if source_bytes[pos..pos + unit].iter().all(|&byte| byte == 0) {
                        nul_pos = Some(pos);
                        break;
                    }
                    pos += unit;
                }
                let end = nul_pos.unwrap_or(source_bytes.len());
                let mut offsets = vec![first];
                let mut next = after_first;
                while let Some(found) = memchr::memmem::find(&source_bytes[next..end], placeholder)
                {
                    offsets.push(next + found);
                    next += found + placeholder.len();
                }
                covered_until[alignment] = end;
                let rank = match (&prefix.encoding, alignment == 0, nul_pos.is_some()) {
                    (OffsetEncoding::Utf8, _, _) => 0u8,
                    (_, true, true) => 1,
                    (_, true, false) => 2,
                    (_, false, true) => 3,
                    (_, false, false) => 4,
                };
                candidates.push((rank, offsets, end, prefix.encoding));
            }
        }
        candidates.sort_by_key(|(rank, offsets, ..)| (*rank, offsets[0]));
        let mut kept: BTreeMap<usize, (usize, Vec<usize>, OffsetEncoding)> = BTreeMap::new();
        for (_, offsets, end, encoding) in candidates {
            let start = offsets[0];
            let overlaps_earlier = kept
                .range(..=start)
                .next_back()
                .is_some_and(|(_, earlier)| start < earlier.0);
            let overlaps_later = kept
                .range(start..)
                .next()
                .is_some_and(|(&later_start, _)| later_start < end);
            if !overlaps_earlier && !overlaps_later {
                kept.insert(start, (end, offsets, encoding));
            }
        }
        kept.into_iter()
            .map(|(start, (end, offsets, encoding))| (start, end, offsets, encoding))
            .collect()
    }

    fn patches(
        source_bytes: &[u8],
        prefixes: &[EncodedPrefix],
    ) -> Vec<(usize, usize, Vec<usize>, OffsetEncoding)> {
        find_cstring_patches(source_bytes, prefixes)
            .into_iter()
            .map(|patch| {
                (
                    patch.offsets[0],
                    patch.nul_pos,
                    patch.offsets.into_owned(),
                    patch.prefix.encoding,
                )
            })
            .collect()
    }

    /// Placeholders covering the single-pass search (including self-overlapping ones, whose
    /// leftmost non-overlapping matches differ from all matches) and the fallback.
    const PLACEHOLDERS: &[&str] = &[
        "/pfx/pfx",
        "abab",
        "/aa",
        "_ph_ph_ph",
        "/opt/é",
        "/opt/€",
        "/",
    ];

    fn source() -> impl Strategy<Value = (usize, Vec<u8>)> {
        let placeholder = 0..PLACEHOLDERS.len();
        placeholder.prop_flat_map(|index| {
            let text = PLACEHOLDERS[index];
            let chunk = prop_oneof![
                3 => (0usize..5).prop_map(move |encoding| OffsetEncoding::DEFINED[encoding].encode(text)),
                // Fragments of the placeholder, so near-misses and overlaps occur.
                2 => (0usize..5, 0usize..8).prop_map(move |(encoding, cut)| {
                    let bytes = OffsetEncoding::DEFINED[encoding].encode(text);
                    bytes[..cut.min(bytes.len())].to_vec()
                }),
                3 => proptest::collection::vec(prop_oneof![4 => Just(0u8), 1 => any::<u8>(), 2 => proptest::sample::select(text.as_bytes().to_vec())], 0..6),
            ];
            (Just(index), proptest::collection::vec(chunk, 0..24).prop_map(|chunks| chunks.concat()))
        })
    }

    proptest! {
        #[test]
        fn single_pass_matches_per_encoding_search((index, source) in source()) {
            let prefixes = EncodedPrefix::all(PLACEHOLDERS[index], "");
            let per_encoding: Vec<Vec<usize>> = prefixes
                .iter()
                .map(|prefix| memchr::memmem::find_iter(&source, &prefix.placeholder).collect())
                .collect();
            prop_assert_eq!(find_occurrences(&source, &prefixes), per_encoding);
            prop_assert_eq!(patches(&source, &prefixes), reference_patches(&source, &prefixes));
        }
    }

    #[test]
    fn single_pass_is_used_for_latin1_placeholders() {
        let prefixes = EncodedPrefix::all("/opt/é", "");
        assert!(find_occurrences_single_pass(b"", &prefixes).is_some());
        let prefixes = EncodedPrefix::all("/opt/€", "");
        assert!(find_occurrences_single_pass(b"", &prefixes).is_none());
    }
}
