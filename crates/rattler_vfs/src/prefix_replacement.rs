use memchr::memmem;
use memmap2::Mmap;
use std::{os::unix::ffi::OsStrExt, path::PathBuf};

use crate::metadata::PrefixReplacement;

pub fn text_prefix_replacement(
    placeholder: &PrefixReplacement,
    start: usize,
    _end: usize,
    size: usize,
    file: &Mmap,
    mount_point: &PathBuf,
) -> Vec<u8> {
    if start >= file.len() {
        return vec![];
    }

    let old_prefix = placeholder.placeholder.as_bytes();
    let new_prefix = mount_point.as_os_str().as_bytes();

    if new_prefix.len() > old_prefix.len() {
        panic!("New prefix is longer than placeholder");
    }

    let mut replaced = Vec::with_capacity(file.len());
    let mut last_pos = 0;

    for &offset in &placeholder.offsets {
        if offset > file.len() {
            continue;
        }

        replaced.extend_from_slice(&file[last_pos..offset]);
        replaced.extend_from_slice(new_prefix);
        last_pos = offset + old_prefix.len();
    }

    if last_pos < file.len() {
        replaced.extend_from_slice(&file[last_pos..]);
    }

    let end = start.saturating_add(size).min(replaced.len());
    replaced[start..end].to_vec()
}

pub fn binary_prefix_replacement(
    placeholder: &PrefixReplacement,
    start: usize,
    end: usize,
    _size: usize,
    file: &Mmap,
    mount_point: &PathBuf,
) -> Vec<u8> {
    // Maybe check if the end is not later than the end
    // Using this method will replace/ read byte by byte, it might be better to create a function which handles this more efficiently?
    //  ? storing nul bytes in another list

    let new_prefix = mount_point.as_os_str().as_bytes();
    let length_placeholder = placeholder.placeholder.len();
    let length_prefix = new_prefix.len();

    // Handle underflow: use checked subtraction or i64
    if length_prefix > length_placeholder {
        panic!("New prefix is longer than placeholder");
    }
    let length_change = length_placeholder - length_prefix;

    if start >= end || start >= file.len() {
        return vec![];
    }

    let length = end - start;
    let mut buffer = vec![0u8; length];
    let mut buffer_pos = 0;

    let mut next_placeholder_index = match placeholder.offsets.binary_search(&start) {
        Ok(index) => index,
        Err(index) => index,
    };

    // should be actual start
    let mut unfinished_replacements = if next_placeholder_index >= 1 {
        let placeholders_before = &placeholder.offsets[0..next_placeholder_index];
        find_unfinished_replacements(file[0..start].to_vec(), placeholders_before.to_vec())
    } else {
        0
    };

    let actual_start = if unfinished_replacements >= 1 {
        start + (unfinished_replacements * length_change)
    } else {
        start
    };

    let mut file_pos = actual_start;

    while file_pos < end && buffer_pos < length {
        let next_placeholder = if next_placeholder_index < placeholder.offsets.len() {
            placeholder.offsets[next_placeholder_index]
        } else {
            end
        };

        // Only process if we've reached a placeholder within our range
        if file_pos == next_placeholder && next_placeholder < end {
            next_placeholder_index += 1;

            // Copy the new prefix
            let copy_len = length_prefix.min(length - buffer_pos);
            buffer[buffer_pos..buffer_pos + copy_len].copy_from_slice(&new_prefix[..copy_len]);
            buffer_pos += copy_len;
            unfinished_replacements += 1;

            if buffer_pos >= length {
                return buffer;
            }

            // Skip the old placeholder in the file
            file_pos += length_placeholder;

            if file_pos >= file.len() || file_pos >= end {
                break;
            }

            // Get next placeholder position for boundary checking
            let following_placeholder = if next_placeholder_index < placeholder.offsets.len() {
                placeholder.offsets[next_placeholder_index]
            } else {
                end
            };

            // Copy until null byte, next placeholder, or end
            while file_pos < file.len()
                && file_pos < end
                && file_pos < following_placeholder
                && file[file_pos] != b'\x00'
                && buffer_pos < length
            {
                buffer[buffer_pos] = file[file_pos];
                buffer_pos += 1;
                file_pos += 1;
            }

            // If we hit a null byte, copy it & add the padding after the string content
            if file_pos < file.len() && file_pos < end && file[file_pos] == b'\x00' {
                buffer_pos += unfinished_replacements * length_change;
                unfinished_replacements = 0;
            }
        } else if file[file_pos] == b'\x00' && next_placeholder < end && unfinished_replacements > 0
        {
            // buffer_pos += 1; // the already existing null byte
            buffer_pos += unfinished_replacements * length_change;
            unfinished_replacements = 0;
        } else {
            // Regular copy
            buffer[buffer_pos] = file[file_pos];
            buffer_pos += 1;
            file_pos += 1;
        }
    }
    buffer
}

/// Within the prefix replacement function
pub fn find_unfinished_replacements(file_before: Vec<u8>, offsets: Vec<usize>) -> usize {
    // there is at least one offset before
    let last_nul_byte = match memmem::rfind(&file_before, b"\x00") {
        Some(last_nul_byte) => last_nul_byte,
        None => 0,
    };
    if offsets.last().unwrap() < &last_nul_byte {
        // the last 0 byte is after the last prefix meaning there is no unfinished replacement
        return 0;
    }
    let mut unfinished_replacements = 0;
    let reversed_offsets: Vec<usize> = offsets.into_iter().rev().collect();
    for offset in reversed_offsets {
        if offset >= last_nul_byte {
            unfinished_replacements += 1;
        } else {
            return unfinished_replacements;
        }
    }
    unfinished_replacements
}

#[cfg(test)]
mod tests {
    use memmap2::MmapOptions;
    use rattler_conda_types::package::{FileMode, PrefixPlaceholder};

    use super::*;

    fn mmap_bytes(bytes: &[u8]) -> Mmap {
        let mut mmap = MmapOptions::new().len(bytes.len()).map_anon().unwrap();
        mmap[..].copy_from_slice(bytes);
        mmap.make_read_only().unwrap()
    }

    fn replacement(file_mode: FileMode, placeholder: &str, bytes: &[u8]) -> PrefixReplacement {
        PrefixReplacement::from_placeholder(
            PrefixPlaceholder {
                file_mode,
                placeholder: placeholder.to_string(),
            },
            bytes,
        )
    }

    #[test]
    fn metadata_collects_prefix_offsets() {
        let bytes = b"ABCD0123ABCD";
        let replacement = replacement(FileMode::Text, "ABCD", bytes);

        assert_eq!(replacement.offsets, vec![0, 8]);
    }

    #[test]
    fn text_replacement_returns_shortened_virtual_range() {
        let bytes = b"ABCD0ABCD5ABCD0";
        let mmap = mmap_bytes(bytes);
        let replacement = replacement(FileMode::Text, "ABCD", bytes);
        let mount_point = PathBuf::from("XY");

        let result = text_prefix_replacement(&replacement, 2, 9, 7, &mmap, &mount_point);

        assert_eq!(result, b"0XY5XY0");
    }

    #[test]
    fn binary_replacement_pads_shorter_prefix_with_nuls() {
        let bytes = b"ABCD\x000ABCD\x00";
        let mmap = mmap_bytes(bytes);
        let replacement = replacement(FileMode::Binary, "ABCD", bytes);
        let mount_point = PathBuf::from("XY");

        let result = binary_prefix_replacement(
            &replacement,
            0,
            bytes.len(),
            bytes.len(),
            &mmap,
            &mount_point,
        );

        assert_eq!(result, b"XY\x00\x00\x000XY\x00\x00\x00");
    }
}
