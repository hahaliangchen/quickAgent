    use super::*;

    #[test]
    fn to_fixed_rounds_the_exact_binary_value_half_up() {
        // Expected strings come from Node.js 24.
        for (value, digits, expected) in [
            (1.005, 2, "1.00"),
            (2.5, 0, "3"),
            (0.5, 0, "1"),
            (-1.5, 0, "-2"),
            (1234.5678, 1, "1234.6"),
            (1e21, 2, "1e+21"),
            (0.000001, 7, "0.0000010"),
            (-0.0001, 2, "0.00"),
        ] {
            assert_eq!(to_fixed(value, digits), expected, "{value}.toFixed({digits})");
        }
    }

    #[test]
    fn substitution_patterns_match_javascript() {
        let text = "aXbXc";
        assert_eq!(substitution("[$&]", text, 1, 2, &[], &[]), "[X]");
        assert_eq!(substitution("$$", text, 1, 2, &[], &[]), "$");
        assert_eq!(substitution("$1", "x", 0, 1, &[], &[]), "$1");
        assert_eq!(substitution("<$1>", "a1", 1, 2, &[Some("1".into())], &[]), "<1>");
        assert_eq!(substitution("$`|$'", text, 1, 2, &[], &[]), "a|bXc");
    }

    #[test]
    fn number_parsing_globals_match_javascript() {
        assert_eq!(parse_int("  42px", 0.0), 42.0);
        assert_eq!(parse_int("0x1F", 0.0), 31.0);
        assert_eq!(parse_int("1F", 16.0), 31.0);
        assert_eq!(parse_int("z", 36.0), 35.0);
        assert!(parse_int("", 0.0).is_nan());
        assert_eq!(parse_int("-12.9", 0.0), -12.0);
        assert_eq!(parse_float("3.14abc"), 3.14);
        assert_eq!(parse_float(".5e2x"), 50.0);
        assert_eq!(parse_float("Infinityx"), f64::INFINITY);
        assert_eq!(locale_string(1234567.891), "1,234,567.891");
        assert_eq!(locale_string(0.1), "0.1");
        assert_eq!(radix_string(255.0, 16), "ff");
        assert_eq!(radix_string(255.0, 2), "11111111");
    }
