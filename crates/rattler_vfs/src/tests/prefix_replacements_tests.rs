#[cfg(test)]
mod tests {
    use crate::{
        prefix_placeholder::PrefixPlaceholder,
        prefix_replacement::{
            binary_prefix_replacement, find_unfinished_replacements, text_prefix_replacement,
        },
    };
    use memmap2::MmapOptions;
    use rattler_conda_types::package::FileMode;
    use std::path::PathBuf;

    #[test]
    fn test_find_one_unfinished_replacements() {
        let file_before = b"01ABCD2\x0034ABCD5";
        let offsets = vec![2, 10];

        let expected_unfinished_replacements = 1;
        let created_unfinished_replacements =
            find_unfinished_replacements(file_before.to_vec(), offsets.clone());
        assert_eq!(
            expected_unfinished_replacements, created_unfinished_replacements,
            "unfinished replacements failed for {:?}, expected unfinished replacements {expected_unfinished_replacements:?}, actual unfinished_replacements {created_unfinished_replacements:?}",
            &offsets
        );
    }

    #[test]
    fn test_find_two_unfinished_replacements_no_null_byte() {
        let file_before = b"01ABCD234ABCD5";
        let offsets = vec![2, 9];

        let expected_unfinished_replacements = 2;
        let created_unfinished_replacements =
            find_unfinished_replacements(file_before.to_vec(), offsets.clone());
        assert_eq!(
            expected_unfinished_replacements, created_unfinished_replacements,
            "unfinished replacements failed for {:?}, expected unfinished replacements {expected_unfinished_replacements:?}, actual unfinished_replacements {created_unfinished_replacements:?}",
            &offsets
        );
    }

    fn do_text_test(
        placeholder: &str,
        prefix: &str,
        before: &[u8],
        expected: &[u8],
        start: usize,
        end: usize,
    ) {
        let mut placeholder_obj =
            PrefixPlaceholderV2::new(FileMode::Text, placeholder.as_bytes().to_vec(), offsets);
        let size = before.len();

        let mut file = MmapOptions::new().len(before.len()).map_anon().unwrap();
        file[0..before.len()].copy_from_slice(before);
        let file = file.make_read_only().unwrap();
        placeholder_obj.fill_offsets(&file);
        let mount_point = PathBuf::from(prefix);

        let created_buffer;
        text_prefix_replacement(
            before,
            created_buffer,
            placeholder_obj.placeholder,
            mount_point,
            offsets,
        );
        // source_bytes: &[u8],
        // mut destination: impl Write,
        // prefix_placeholder: &str,
        // target_prefix: &str,
        // offsets: Vec<usize>
        assert_eq!(
            created_buffer, expected,
            "replacement failed for {before:?} to expected: {expected:?}, {start} to {end}"
        );
    }

    fn do_binary_test(
        placeholder: &str,
        prefix: &str,
        before: &[u8],
        expected: &[u8],
        start: usize,
        end: usize,
    ) {
        let mut placeholder_obj =
            PrefixPlaceholder::new(FileMode::Binary, placeholder.as_bytes().to_vec());
        let size = before.len();

        let mut file = MmapOptions::new().len(before.len()).map_anon().unwrap();
        file[0..before.len()].copy_from_slice(before);
        let file = file.make_read_only().unwrap();
        placeholder_obj.fill_offsets(&file);
        let mount_point = PathBuf::from(prefix);

        let created_buffer =
            binary_prefix_replacement(&placeholder_obj, start, end, size, &file, &mount_point);
        assert_eq!(
            created_buffer, expected,
            "replacement failed for {before:?} to expected: {expected:?}, {start} to {end}"
        );
    }

    #[test]
    fn test_binary_replacement_full_file_multiple_placeholders() {
        let placeholder = "ABCD";
        let prefix = "XY";
        let before = b"\x00\x00ABCDZ\x00\x00\x00ABCDEFABCDEF\x00\x00\x00ABCDMNOPQRSABCDMNOPQRSABCDMNOPQRS\x00\x00";
        let start = 0;
        let end = before.len();

        let expected = b"\x00\x00XYZ\x00\x00\x00\x00\x00XYEFXYEF\x00\x00\x00\x00\x00\x00\x00XYMNOPQRSXYMNOPQRSXYMNOPQRS\x00\x00\x00\x00\x00\x00\x00\x00";
        do_binary_test(placeholder, prefix, before, expected, start, end);
    }

    #[test]
    fn test_text_prefix_replacement_full_file() {
        do_text_test(
            "ABCD",
            "XY",
            b"01ABCD23456ABCD7890",
            b"01XY23456XY7890",
            0,
            b"01ABCD23456ABCD7890".len(),
        );
    }

    #[test]
    fn test_binary_prefix_replacement_full_file() {
        do_binary_test(
            "ABCD",
            "XY",
            b"01ABCD23\x00456ABCD78\x0090",
            b"01XY23\x00\x00\x00456XY78\x00\x00\x0090",
            0,
            b"01ABCD23\x00456ABCD78\x0090".len(),
        );
    }

    #[test]
    fn test_text_prefix_replacement_partial_range() {
        // Replace only a portion of the file
        let placeholder = "ABCD";
        let prefix = "XY";
        let before = b"ABCD0ABCD5ABCD0ABCD5ABCD";
        let start = 2;
        let end = 9; // Only process middle section

        let expected = b"0XY5XY0";
        do_text_test(placeholder, prefix, before, expected, start, end);
    }

    #[test]
    fn test_binary_prefix_replacement_partial_range() {
        // Replace only a portion of the file
        let placeholder = "ABCD";
        let prefix = "XY";
        let before = b"ABCD\x000ABCD\x005ABCD\x000ABCD\x005ABCD\x00";
        let start = 5;
        let end = 10; // Only process middle section

        let expected = b"0XY\x00\x00";
        do_binary_test(placeholder, prefix, before, expected, start, end);
    }

    #[test]
    fn test_text_prefix_replacement_start_after_prefix() {
        let placeholder = "ABCD";
        let prefix = "XY";
        let before = b"ABCD01234ABCD56789";
        let expected = b"34XY56789";

        let mut placeholder_obj =
            PrefixPlaceholder::new(FileMode::Text, placeholder.as_bytes().to_vec());
        let start = 5;
        let end = before.len();
        let size = before.len();

        let mut file = MmapOptions::new().len(end).map_anon().unwrap();
        file[0..end].copy_from_slice(before);
        let file = file.make_read_only().unwrap();
        placeholder_obj.fill_offsets(&file);
        let mount_point = PathBuf::from(prefix);

        let created_buffer =
            text_prefix_replacement(&placeholder_obj, start, end, size, &file, &mount_point);
        assert_eq!(created_buffer, expected, "Start after prefix failed");
    }

    #[test]
    fn test_binary_prefix_replacement_start_after_prefix() {
        let placeholder = "ABCD";
        let prefix = "XY";
        let before = b"ABCD01234ABCD\x0056789";
        let expected = b"34XY\x00\x00\x00\x00\x0056789";

        let mut placeholder_obj =
            PrefixPlaceholder::new(FileMode::Binary, placeholder.as_bytes().to_vec());
        let start = 5;
        let end = before.len();
        let size = before.len();

        let mut file = MmapOptions::new().len(end).map_anon().unwrap();
        file[0..end].copy_from_slice(before);
        let file = file.make_read_only().unwrap();
        placeholder_obj.fill_offsets(&file);
        let mount_point = PathBuf::from(prefix);

        let created_buffer =
            binary_prefix_replacement(&placeholder_obj, start, end, size, &file, &mount_point);
        assert_eq!(created_buffer, expected, "Start after prefix failed");
    }

    #[test]
    fn test_text_prefix_replacement_start_between_placeholders() {
        // Start in the middle, between two placeholders
        let placeholder = "ABCD";
        let prefix = "XY";
        let before = b"ABCD0123ABCD5678ABCD";
        let expected = b"3XY5678XY"; // Starting at position 7

        let mut placeholder_obj =
            PrefixPlaceholder::new(FileMode::Text, placeholder.as_bytes().to_vec());
        let start = 5;
        let end = before.len();
        let size = end - start;

        let mut file = MmapOptions::new().len(end).map_anon().unwrap();
        file[0..end].copy_from_slice(before);
        let file = file.make_read_only().unwrap();
        placeholder_obj.fill_offsets(&file);
        let mount_point = PathBuf::from(prefix);

        let created_buffer =
            text_prefix_replacement(&placeholder_obj, start, end, size, &file, &mount_point);
        assert_eq!(
            created_buffer, expected,
            "Start between placeholders failed"
        );
    }

    #[test]
    fn test_binary_prefix_replacement_start_between_placeholders() {
        // Start in the middle, between two placeholders
        let placeholder = "ABCD";
        let prefix = "XY";
        let before = b"ABCD012\x003ABCD5678ABCD";
        let expected = b"012\x00\x00\x003XY5678XY\x00\x00\x00\x00"; // Starting at position 7

        let mut placeholder_obj =
            PrefixPlaceholder::new(FileMode::Binary, placeholder.as_bytes().to_vec());
        let start = 2;
        let end = before.len();
        let size = end - start;

        let mut file = MmapOptions::new().len(end).map_anon().unwrap();
        file[0..end].copy_from_slice(before);
        let file = file.make_read_only().unwrap();
        placeholder_obj.fill_offsets(&file);
        let mount_point = PathBuf::from(prefix);

        let created_buffer =
            binary_prefix_replacement(&placeholder_obj, start, end, size, &file, &mount_point);
        assert_eq!(
            created_buffer, expected,
            "Start between placeholders failed"
        );
    }

    #[test]
    fn test_text_prefix_replacement_start_at_placeholder() {
        // Start exactly at a placeholder position
        do_text_test(
            "ABCD",
            "XY",
            b"01234ABCD6789ABCD",
            b"XY6789XY",
            5,
            b"01234ABCD6789ABCD".len(),
        );
    }

    #[test]
    fn test_binary_prefix_replacement_start_at_placeholder() {
        // Start exactly at a placeholder position
        do_binary_test(
            "ABCD",
            "XY",
            b"01234ABCD\x006789ABCD\x00",
            b"XY\x00\x00\x006789XY\x00\x00\x00",
            5,
            b"01234ABCD\x006789ABCD\x00".len(),
        );
    }

    #[test]
    fn test_text_prefix_replacement_no_placeholders() {
        do_text_test(
            "ABCD",
            "XY",
            b"0123456789",
            b"0123456789",
            0,
            b"0123456789".len(),
        );
    }

    #[test]
    fn test_binary_prefix_replacement_no_placeholders() {
        do_binary_test(
            "ABCD",
            "XY",
            b"0123456789",
            b"0123456789",
            0,
            b"0123456789".len(),
        );
    }

    #[test]
    fn test_text_prefix_replacement_only_placeholder() {
        do_text_test("ABCD", "XY", b"ABCD", b"XY", 0, b"ABCD".len());
    }

    #[test]
    fn test_binary_prefix_replacement_only_placeholder() {
        do_binary_test("ABCD", "XY", b"ABCD", b"XY\x00\x00", 0, b"ABCD".len());
    }

    #[test]
    fn test_text_prefix_replacement_start_with_placeholder() {
        do_text_test(
            "ABCD",
            "XY",
            b"ABCD01234",
            b"XY01234",
            0,
            b"ABCD01234".len(),
        );
    }

    #[test]
    fn test_binary_prefix_replacement_start_with_placeholder() {
        do_binary_test(
            "ABCD",
            "XY",
            b"ABCD\x0001234",
            b"XY\x00\x00\x0001234",
            0,
            b"ABCD\x0001234".len(),
        );
    }

    #[test]
    fn test_text_prefix_replacement_end_with_placeholder() {
        do_text_test(
            "ABCD",
            "XY",
            b"01234ABCD",
            b"01234XY",
            0,
            b"01234ABCD".len(),
        );
    }

    #[test]
    fn test_binary_prefix_replacement_end_with_placeholder() {
        do_binary_test(
            "ABCD",
            "XY",
            b"01234ABCD",
            b"01234XY\x00\x00",
            0,
            b"01234ABCD".len(),
        );
    }

    #[test]
    fn test_text_prefix_replacement_consecutive_placeholders() {
        do_text_test("ABCD", "XY", b"ABCDABCD", b"XYXY", 0, b"ABCDABCD".len());
    }

    #[test]
    fn test_binary_prefix_replacement_consecutive_placeholders() {
        do_binary_test(
            "ABCD",
            "XY",
            b"ABCDABCD",
            b"XYXY\x00\x00\x00\x00",
            0,
            b"ABCDABCD".len(),
        );
    }

    #[test]
    fn test_text_prefix_replacement_same_length() {
        do_text_test(
            "ABCD",
            "WXYZ",
            b"01ABCD6789012ABCD7890",
            b"01WXYZ6789012WXYZ7890",
            0,
            b"01ABCD6789012ABCD7890".len(),
        );
    }

    #[test]
    fn test_binary_prefix_replacement_same_length() {
        do_binary_test(
            "ABCD",
            "WXYZ",
            b"01ABCD6789012ABCD7890",
            b"01WXYZ6789012WXYZ7890",
            0,
            b"01ABCD6789012ABCD7890".len(),
        );
    }

    #[test]
    fn test_text_prefix_replacement_empty_file() {
        do_text_test("ABCD", "XY", b"", b"", 0, b"".len());
    }

    #[test]
    fn test_binary_prefix_replacement_empty_file() {
        do_binary_test("ABCD", "XY", b"", b"", 0, b"".len());
    }

    #[test]
    fn test_text_prefix_replacement_single_char_placeholder() {
        do_text_test("X", "A", b"0X2X4X6X8", b"0A2A4A6A8", 0, b"0X2X4X6X8".len());
    }

    #[test]
    fn test_binary_prefix_replacement_single_char_placeholder() {
        do_binary_test("X", "A", b"0X2X4X6X8", b"0A2A4A6A8", 0, b"0X2X4X6X8".len());
    }

    #[test]
    fn test_text_prefix_replacement_many_placeholders() {
        let mut before = Vec::new();
        let mut expected = Vec::new();
        for i in 0..10 {
            before.extend_from_slice(format!("{:02}ABCD", i).as_bytes());
            expected.extend_from_slice(format!("{:02}XY", i).as_bytes());
        }
        do_text_test("ABCD", "XY", &before, &expected, 0, before.len());
    }

    #[test]
    fn test_binary_prefix_replacement_many_placeholders() {
        let mut before = Vec::new();
        let mut expected = Vec::new();
        for i in 0..10 {
            before.extend_from_slice(format!("{:02}ABCD\x00", i).as_bytes());
            expected.extend_from_slice(format!("{:02}XY\x00\x00\x00", i).as_bytes());
        }
        do_binary_test("ABCD", "XY", &before, &expected, 0, before.len());
    }

    #[test]
    fn test_text_prefix_replacement_longer_prefix() {
        // Test with a longer replacement (should still work if placeholder is longer)
        do_text_test(
            "ABCDEFGH",
            "XYZ",
            b"01ABCDEFGH234ABCDEFGH567",
            b"01XYZ234XYZ567",
            0,
            b"01ABCDEFGH234ABCDEFGH567".len(),
        );
    }

    #[test]
    fn test_binary_prefix_replacement_longer_prefix() {
        // Test with a longer replacement (should still work if placeholder is longer)
        do_binary_test(
            "ABCDEFGH",
            "XYZ",
            b"01ABCDEFGH234ABCDEFGH567",
            b"01XYZ234XYZ567\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00",
            0,
            b"01ABCDEFGH234ABCDEFGH567".len(),
        );
    }

    #[test]
    fn test_text_prefix_replacement_three_char_to_one() {
        do_text_test(
            "ABC",
            "X",
            b"00ABC11ABC22ABC33",
            b"00X11X22X33",
            0,
            b"00ABC11ABC22ABC33".len(),
        );
    }

    #[test]
    fn test_binary_prefix_replacement_three_char_to_one() {
        do_binary_test(
            "ABC",
            "X",
            b"00ABC11ABC22ABC33",
            b"00X11X22X33\x00\x00\x00\x00\x00\x00",
            0,
            b"00ABC11ABC22ABC33".len(),
        );
    }

    #[test]
    fn test_text_prefix_replacement_with_special_chars() {
        let before = b"{\n  \"path\": \"ABCD/file\",\n  \"root\": \"ABCD\"\n}";
        do_text_test(
            "ABCD",
            "XY",
            before,
            b"{\n  \"path\": \"XY/file\",\n  \"root\": \"XY\"\n}",
            0,
            before.len(),
        );
    }

    #[test]
    fn test_text_prefix_replacement_placeholder_at_boundary() {
        // Placeholder right at the end boundary
        let placeholder = "ABCD";
        let prefix = "XY";
        let before = b"01234567ABCD";
        let start = 0;
        let end = 12; // Includes the placeholder

        do_text_test(placeholder, prefix, before, b"01234567XY", start, end);
    }

    #[test]
    fn test_binary_prefix_replacement_placeholder_at_boundary() {
        // Placeholder right at the end boundary
        let placeholder = "ABCD";
        let prefix = "XY";
        let before = b"01234567ABCD";
        let start = 0;
        let end = 12; // Includes the placeholder

        do_binary_test(
            placeholder,
            prefix,
            before,
            b"01234567XY\x00\x00",
            start,
            end,
        );
    }
}
