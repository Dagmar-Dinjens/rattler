// use memchr::memmem;
// use memmap2::Mmap;
// use rattler_conda_types::package::FileMode;

// #[derive(Clone, Debug)]
// pub struct PrefixPlaceholder {
//     pub file_mode: FileMode,

//     pub placeholder: Vec<u8>,

//     pub offsets: Vec<usize>,
//     // pub permissions: Option<bool>,
//     // pub pos: Option<u64> // with default being 0 and End of file being None
// }

// impl PrefixPlaceholder {
//     pub fn new(file_mode: FileMode, placeholder: Vec<u8>) -> Self {
//         PrefixPlaceholder {
//             file_mode,
//             placeholder,
//             offsets: vec![],
//             // permissions: None,
//             // pos: Some(0)
//         }
//     }

//     // pub fn advance(&mut self, position: u64) {
//     //     assert!(position > self.pos.expect("Can't advance after end of file"), "Can't advance backwards");
//     //     self.pos = Some(position);
//     // }

//     // pub fn advance_eof(&mut self) {
//     //     // self.pos = None;
//     // }

//     pub fn fill_offsets(&mut self, open_file: &Mmap) {
//         // read through the open file and fill in the offsets to the offsets vector
//         let mut offsets = memmem::find_iter(open_file, &self.placeholder).collect();
//         self.offsets.append(&mut offsets);
//     }
// }

// #[cfg(test)]
// mod tests {
//     use super::*;
//     use memmap2::Mmap;
//     use std::{io::Write, vec};
//     use tempfile::NamedTempFile;

//     // Helper function to create a memory-mapped file from a byte slice
//     fn create_mmap_from_bytes(content: &[u8]) -> (NamedTempFile, Mmap) {
//         let mut temp_file = NamedTempFile::new().unwrap();
//         temp_file.write_all(content).unwrap();
//         temp_file.flush().unwrap();

//         let file = temp_file.reopen().unwrap();
//         let mmap = unsafe { Mmap::map(&file).unwrap() };

//         (temp_file, mmap)
//     }

//     #[test]
//     fn test_fill_offsets_with_binary_data() {
//         // let content = vec![0x00, 0xFF, 0xAB, 0xCD, 0x00, 0xFF, 0xAB];
//         let content = b"\x00\xFF\xAB\xCD\x00\xFF\xAB";
//         let (_temp, mmap) = create_mmap_from_bytes(content);

//         let pattern = b"\xFF\xAB";
//         let mut placeholder = PrefixPlaceholder::new(FileMode::Binary, pattern.to_vec());

//         placeholder.fill_offsets(&mmap);

//         // Verify pattern occurrences
//         assert_eq!(
//             placeholder.offsets,
//             vec![1, 5],
//             "Pattern 0xFF 0xAB should appear at offsets 1 and 5"
//         );
//     }
//     #[test]
//     fn test_new_creates_instance_with_initial_state() {
//         let placeholder =
//             PrefixPlaceholder::new(FileMode::Binary, "placeholder".as_bytes().to_vec());

//         assert_eq!(placeholder.placeholder, "placeholder".as_bytes());
//         assert_eq!(placeholder.offsets.len(), 0);
//         // assert_eq!(placeholder.pos, Some(0));
//     }

//     // #[test]
//     // fn test_advance_eof_sets_pos_to_none() {
//     //     let mut placeholder = PrefixPlaceholder::new(
//     //         FileMode::Text,
//     //         "test".as_bytes().to_vec()
//     //     );

//     //     placeholder.advance_eof();
//     //     assert_eq!(placeholder.pos, None);
//     // }

//     #[test]
//     fn test_fill_offsets_finds_single_occurrence() {
//         let content = b"Hello PLACEHOLDER world";
//         let (_temp, mmap) = create_mmap_from_bytes(content);

//         let mut placeholder =
//             PrefixPlaceholder::new(FileMode::Text, "PLACEHOLDER".as_bytes().to_vec());

//         placeholder.fill_offsets(&mmap);

//         assert_eq!(placeholder.offsets.len(), 1);
//         assert_eq!(placeholder.offsets[0], 6);
//         // assert_eq!(placeholder.pos, None);
//     }

//     #[test]
//     fn test_fill_offsets_finds_multiple_occurrences() {
//         let content = b"XXX test XXX another XXX end";
//         let (_temp, mmap) = create_mmap_from_bytes(content);

//         let mut placeholder = PrefixPlaceholder::new(FileMode::Text, "XXX".as_bytes().to_vec());

//         placeholder.fill_offsets(&mmap);

//         assert_eq!(placeholder.offsets.len(), 3);
//         assert_eq!(placeholder.offsets[0], 0);
//         assert_eq!(placeholder.offsets[1], 9);
//         assert_eq!(placeholder.offsets[2], 21);
//     }

//     #[test]
//     fn test_fill_offsets_finds_overlapping_patterns() {
//         let content = b"AAAA";
//         let (_temp, mmap) = create_mmap_from_bytes(content);

//         let mut placeholder = PrefixPlaceholder::new(FileMode::Text, "AA".as_bytes().to_vec());

//         placeholder.fill_offsets(&mmap);

//         // memmem::find_iter finds non-overlapping matches
//         assert!(placeholder.offsets.len() >= 1);
//     }

//     #[test]
//     fn test_fill_offsets_with_no_matches() {
//         let content = b"This file has no placeholders";
//         let (_temp, mmap) = create_mmap_from_bytes(content);

//         let mut placeholder =
//             PrefixPlaceholder::new(FileMode::Text, "NOTFOUND".as_bytes().to_vec());

//         placeholder.fill_offsets(&mmap);

//         assert_eq!(placeholder.offsets.len(), 0);
//         // assert_eq!(placeholder.pos, None);
//     }

//     #[test]
//     fn test_fill_offsets_with_empty_file() {
//         let content = b"";
//         let (_temp, mmap) = create_mmap_from_bytes(content);

//         let mut placeholder = PrefixPlaceholder::new(FileMode::Text, "test".as_bytes().to_vec());

//         placeholder.fill_offsets(&mmap);

//         assert_eq!(placeholder.offsets.len(), 0);
//         // assert_eq!(placeholder.pos, None);
//     }

//     #[test]
//     fn test_fill_offsets_at_file_boundaries() {
//         let content = b"PLACEHOLDER";
//         let (_temp, mmap) = create_mmap_from_bytes(content);

//         let mut placeholder =
//             PrefixPlaceholder::new(FileMode::Text, "PLACEHOLDER".as_bytes().to_vec());

//         placeholder.fill_offsets(&mmap);

//         assert_eq!(placeholder.offsets.len(), 1);
//         assert_eq!(placeholder.offsets[0], 0);
//     }
// }
