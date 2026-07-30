use std::fs;

use rish_applets::{AppletContext, AppletError, AppletExecutor, AppletLimits};
use rish_core::{ExecutionOutcome, ExecutionPath, GuestCommand};
use tempfile::TempDir;

fn command(name: &str, args: &[&str], stdin: &[u8]) -> GuestCommand {
    let mut command = GuestCommand::new(name, args.iter().map(ToString::to_string));
    command.stdin = stdin.to_vec();
    command
}

fn run(root: &TempDir, name: &str, args: &[&str], stdin: &[u8]) -> ExecutionOutcome {
    AppletExecutor::new(AppletContext::new(root.path().canonicalize().unwrap()).unwrap())
        .execute(&command(name, args, stdin))
        .unwrap()
}

#[test]
fn cat_numbers_and_squeezes_files_inside_the_sandbox() {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("input"), b"one\n\n\nthree\n").unwrap();
    let output = run(&root, "cat", &["-bs", "input"], b"ignored");
    assert_eq!(output.stdout, b"     1\tone\n\n     2\tthree\n");
    assert!(matches!(
        output.path,
        ExecutionPath::PortableApplet { ref name } if name == "cat"
    ));
}

#[test]
fn head_and_tail_support_line_byte_and_from_start_counts() {
    let root = TempDir::new().unwrap();
    let input = b"one\ntwo\nthree\n";
    assert_eq!(
        run(&root, "head", &["-n", "2"], input).stdout,
        b"one\ntwo\n"
    );
    assert_eq!(run(&root, "head", &["-c3"], input).stdout, b"one");
    assert_eq!(
        run(&root, "tail", &["--lines=+2"], input).stdout,
        b"two\nthree\n"
    );
    assert_eq!(run(&root, "tail", &["-c", "6"], input).stdout, b"three\n");
}

#[test]
fn wc_sort_and_uniq_have_bounded_filter_semantics() {
    let root = TempDir::new().unwrap();
    assert_eq!(
        run(&root, "wc", &["-lwc"], b"one two\nthree\n").stdout,
        b"2 3 14\n"
    );
    assert_eq!(
        run(&root, "sort", &["-nu"], b"10\n2\n2\n").stdout,
        b"2\n10\n"
    );
    assert_eq!(
        run(&root, "uniq", &["-c"], b"a\na\nb\n").stdout,
        b"      2 a\n      1 b\n"
    );
}

#[test]
fn cut_supports_fields_complements_and_utf8_characters() {
    let root = TempDir::new().unwrap();
    assert_eq!(
        run(&root, "cut", &["-d,", "-f2"], b"a,b,c\n").stdout,
        b"b\n"
    );
    assert_eq!(
        run(&root, "cut", &["--complement", "-b", "2-3"], b"abcde\n").stdout,
        b"ade\n"
    );
    assert_eq!(
        run(&root, "cut", &["--characters=2"], "甲乙\n".as_bytes()).stdout,
        "乙\n".as_bytes()
    );
}

#[test]
fn tr_handles_ranges_classes_delete_and_squeeze() {
    let root = TempDir::new().unwrap();
    assert_eq!(run(&root, "tr", &["a-z", "A-Z"], b"a1z\n").stdout, b"A1Z\n");
    assert_eq!(
        run(&root, "tr", &["-d", "[:digit:]"], b"a12b\n").stdout,
        b"ab\n"
    );
    assert_eq!(run(&root, "tr", &["-s", " "], b"a   b").stdout, b"a b");
}

#[test]
fn grep_supports_fixed_patterns_counts_files_and_no_match_status() {
    let root = TempDir::new().unwrap();
    let output = run(&root, "grep", &["-Fin", "needle"], b"x\nNeedle\n");
    assert_eq!(output.exit_code, 0, "stdout={:?}", output.stdout);
    assert_eq!(output.stdout, b"2:Needle\n");
    assert_eq!(
        run(&root, "grep", &["a+b"], b"a+b\naaab\n").stdout,
        b"a+b\n"
    );
    assert_eq!(
        run(&root, "grep", &["-E", "a+b"], b"a+b\naaab\n").stdout,
        b"aaab\n"
    );

    fs::write(root.path().join("yes"), b"x\n").unwrap();
    fs::write(root.path().join("no"), b"y\n").unwrap();
    let output = run(&root, "grep", &["-L", "x", "yes", "no"], b"");
    assert_eq!(output.exit_code, 0);
    assert_eq!(output.stdout, b"no\n");

    fs::write(root.path().join("patterns"), b"").unwrap();
    let output = run(&root, "grep", &["-f", "patterns", "yes"], b"");
    assert_eq!(output.exit_code, 1);
    assert!(output.stdout.is_empty());
}

#[test]
fn tee_writes_and_appends_only_under_the_sandbox() {
    let root = TempDir::new().unwrap();
    assert_eq!(run(&root, "tee", &["log"], b"one\n").stdout, b"one\n");
    assert_eq!(run(&root, "tee", &["-a", "log"], b"two\n").stdout, b"two\n");
    assert_eq!(fs::read(root.path().join("log")).unwrap(), b"one\ntwo\n");

    let error =
        AppletExecutor::new(AppletContext::new(root.path().canonicalize().unwrap()).unwrap())
            .execute(&command("tee", &["../escape"], b"x"))
            .unwrap_err();
    assert!(matches!(error, AppletError::UnsafePath(_)));
}

#[test]
fn unknown_options_and_resource_limit_overruns_fail_closed() {
    let root = TempDir::new().unwrap();
    let executor =
        AppletExecutor::new(AppletContext::new(root.path().canonicalize().unwrap()).unwrap());
    for (name, args) in [
        ("cat", vec!["--raw"]),
        ("head", vec!["--follow"]),
        ("wc", vec!["--files0-from=list"]),
        ("sort", vec!["--random-sort"]),
        ("uniq", vec!["--all-repeated"]),
        ("cut", vec!["--zero-terminated"]),
        ("tr", vec!["--bogus"]),
        ("grep", vec!["--perl-regexp", "x"]),
        ("tee", vec!["--output-error"]),
    ] {
        let error = executor.execute(&command(name, &args, b"x\n")).unwrap_err();
        assert!(matches!(error, AppletError::InvalidArguments { .. }));
    }

    fs::write(root.path().join("large"), b"123456").unwrap();
    let limits = AppletLimits {
        max_input_bytes: 5,
        max_output_bytes: 64,
        ..AppletLimits::default()
    };
    let context = AppletContext::new(root.path().canonicalize().unwrap())
        .unwrap()
        .with_limits(limits)
        .unwrap();
    let error = AppletExecutor::new(context)
        .execute(&command("cat", &["large"], b""))
        .unwrap_err();
    assert!(matches!(error, AppletError::InputLimit { limit: 5 }));

    let limits = AppletLimits {
        max_input_bytes: 64,
        max_output_bytes: 4,
        ..AppletLimits::default()
    };
    let context = AppletContext::new(root.path().canonicalize().unwrap())
        .unwrap()
        .with_limits(limits)
        .unwrap();
    let error = AppletExecutor::new(context)
        .execute(&command("cat", &["-n"], b"x\n"))
        .unwrap_err();
    assert!(matches!(error, AppletError::OutputLimit { limit: 4 }));
}

#[test]
fn tee_cumulative_writes_obey_the_output_limit() {
    let root = TempDir::new().unwrap();
    let context = AppletContext::new(root.path().canonicalize().unwrap())
        .unwrap()
        .with_limits(AppletLimits {
            max_output_bytes: 5,
            ..AppletLimits::default()
        })
        .unwrap();
    let error = AppletExecutor::new(context)
        .execute(&command("tee", &["one", "two"], b"abc"))
        .unwrap_err();

    assert!(matches!(error, AppletError::OutputLimit { limit: 5 }));
}
