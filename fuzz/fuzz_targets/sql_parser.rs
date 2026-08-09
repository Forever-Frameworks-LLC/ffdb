#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|bytes: &[u8]| {
    let Ok(input) = std::str::from_utf8(bytes) else {
        return;
    };

    let classification = ffdb_sql_parser::parse_and_classify_statement(input);
    assert_eq!(
        classification,
        ffdb_sql_parser::parse_and_classify_statement(input),
        "classification must be deterministic"
    );
    let split = ffdb_sql_parser::split_sql_statements(input);
    assert_eq!(
        split,
        ffdb_sql_parser::split_sql_statements(input),
        "statement splitting must be deterministic"
    );
    let rls = ffdb_sql_parser::parse_rls(input);
    assert_eq!(
        rls,
        ffdb_sql_parser::parse_rls(input),
        "multi-statement RLS parsing must be deterministic"
    );
});
