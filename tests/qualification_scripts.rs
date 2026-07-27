use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

const EXPECTED_ICON_SIZES: [u32; 9] = [16, 20, 24, 32, 40, 48, 64, 128, 256];
const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

fn repository_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn run_gate(extra_arguments: &[&str]) -> Output {
    Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(repository_path("tools/Test-WindowsQualification.ps1"))
        .args(extra_arguments)
        .output()
        .expect("the qualification gate script should start")
}

fn run_preview_measurement(extra_arguments: &[&str]) -> Output {
    Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(repository_path("tools/Measure-PreviewWindow.ps1"))
        .args(extra_arguments)
        .output()
        .expect("the preview-window measurement script should start")
}

fn run_fixture_generator(extra_arguments: &[&str]) -> Output {
    Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(repository_path("tools/New-ResolverFixture.ps1"))
        .args(extra_arguments)
        .output()
        .expect("the resolver fixture generator should start")
}

fn run_live_page_collector(extra_arguments: &[&str]) -> Output {
    Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(repository_path("tools/Measure-ExplorerResolverPage.ps1"))
        .args(extra_arguments)
        .output()
        .expect("the live resolver page collector should start")
}

fn run_icon_generator(extra_arguments: &[&str]) -> Output {
    Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(repository_path("tools/New-WindowsIcon.ps1"))
        .args(extra_arguments)
        .output()
        .expect("the Windows icon generator should start")
}

fn run_release_checksums(extra_arguments: &[&str]) -> Output {
    Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(repository_path("tools/New-ReleaseChecksums.ps1"))
        .args(extra_arguments)
        .output()
        .expect("the release checksum generator should start")
}

fn example_arguments() -> Vec<String> {
    vec![
        "-ResolverResults".into(),
        repository_path("qualification/schema-example.resolver.tsv")
            .to_string_lossy()
            .into_owned(),
        "-WindowEvidence".into(),
        repository_path("qualification/schema-example.window.tsv")
            .to_string_lossy()
            .into_owned(),
    ]
}

fn run_owned_arguments(arguments: &[String]) -> Output {
    let borrowed = arguments.iter().map(String::as_str).collect::<Vec<_>>();
    run_gate(&borrowed)
}

fn assert_required_rgba_icon_frames(icon: &[u8]) {
    assert_eq!(read_u16(icon, 0), 0, "ICO reserved field");
    assert_eq!(read_u16(icon, 2), 1, "ICO image type");
    assert_eq!(
        usize::from(read_u16(icon, 4)),
        EXPECTED_ICON_SIZES.len(),
        "ICO directory count"
    );

    let mut actual_sizes = Vec::new();
    for index in 0..EXPECTED_ICON_SIZES.len() {
        let entry = 6 + (index * 16);
        let width = icon_dimension(icon[entry]);
        let height = icon_dimension(icon[entry + 1]);
        assert_eq!(width, height, "ICO entry {index} must be square");
        assert_eq!(icon[entry + 2], 0, "ICO color count");
        assert_eq!(icon[entry + 3], 0, "ICO reserved byte");
        assert_eq!(read_u16(icon, entry + 4), 1, "ICO color planes");
        assert_eq!(read_u16(icon, entry + 6), 32, "ICO bit depth");

        let payload_length = read_u32(icon, entry + 8) as usize;
        let payload_offset = read_u32(icon, entry + 12) as usize;
        let payload_end = payload_offset
            .checked_add(payload_length)
            .expect("ICO payload range should not overflow");
        let payload = icon
            .get(payload_offset..payload_end)
            .expect("ICO payload should remain inside the generated file");

        assert_eq!(&payload[..PNG_SIGNATURE.len()], PNG_SIGNATURE);
        assert_eq!(&payload[12..16], b"IHDR");
        assert_eq!(
            u32::from_be_bytes(payload[16..20].try_into().unwrap()),
            width
        );
        assert_eq!(
            u32::from_be_bytes(payload[20..24].try_into().unwrap()),
            height
        );
        assert_eq!(payload[24], 8, "PNG bit depth");
        assert_eq!(payload[25], 6, "PNG color type must be RGBA");
        actual_sizes.push(width);
    }

    assert_eq!(actual_sizes, EXPECTED_ICON_SIZES);
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(
        bytes[offset..offset + 2]
            .try_into()
            .expect("two-byte field should be present"),
    )
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("four-byte field should be present"),
    )
}

fn icon_dimension(encoded: u8) -> u32 {
    if encoded == 0 {
        256
    } else {
        u32::from(encoded)
    }
}

#[test]
fn windows_icon_generator_emits_required_rgba_frames() {
    let generated = repository_path("target/qualification-tests/CursorPeek.generated.ico");
    if generated.exists() {
        fs::remove_file(&generated).expect("the prior generated icon should be removable");
    }

    let output = run_icon_generator(&[
        "-InputPath",
        repository_path("assets/windows/CursorPeek.png")
            .to_str()
            .expect("the source logo path should be Unicode"),
        "-OutputPath",
        generated
            .to_str()
            .expect("the generated icon path should be Unicode"),
    ]);
    assert!(
        output.status.success(),
        "icon generation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let generated_icon = fs::read(&generated).expect("the generated icon should be readable");
    assert_required_rgba_icon_frames(&generated_icon);
    fs::remove_file(generated).expect("the generated icon should be removable");
}

#[test]
fn nsis_cleanup_sections_are_uninstaller_only() {
    let script = fs::read_to_string(repository_path("packaging/CursorPeek.nsi"))
        .expect("the NSIS installer script should be readable");

    assert!(
        script.contains(r#"Section "un.Remove CursorPeek" un.SecUninstall"#),
        "the required cleanup section name must use NSIS's uninstaller prefix"
    );
    assert!(
        script.contains(r#"Section "un.Remove user settings" un.SecRemoveSettings"#),
        "the optional settings cleanup section name must use NSIS's uninstaller prefix"
    );
}

#[test]
fn release_workflow_resolves_an_annotated_tag_to_its_commit() {
    let workflow = fs::read_to_string(repository_path(".github/workflows/release.yml"))
        .expect("the release workflow should be readable");

    assert!(
        workflow.contains(r#""refs/tags/$($env:GITHUB_REF_NAME)^{commit}""#,),
        "the release guard must peel an annotated tag to its commit"
    );
    assert!(
        workflow.contains("git rev-parse `"),
        "the release guard must use the Git revision verification command"
    );
    assert!(
        !workflow.contains("git rev-list -n 1 --verify"),
        "git rev-list does not support the --verify option"
    );
}

#[test]
fn release_workflow_uses_a_draft_aware_asset_query() {
    let workflow = fs::read_to_string(repository_path(".github/workflows/release.yml"))
        .expect("the release workflow should be readable");

    assert!(
        workflow.contains("gh release view $env:GITHUB_REF_NAME `"),
        "draft verification must use the release command that can resolve unpublished releases"
    );
    assert!(
        workflow.contains("--json 'isDraft,isPrerelease,assets'"),
        "draft verification must request state and asset digests"
    );
    assert!(
        workflow.contains("$release.isDraft") && workflow.contains("$release.isPrerelease"),
        "draft verification must check the field names returned by gh release view"
    );
    assert!(
        !workflow.contains(r#""repos/$env:GITHUB_REPOSITORY/releases/tags/$env:GITHUB_REF_NAME""#,),
        "the release-by-tag REST endpoint does not return draft releases"
    );
}

#[test]
fn release_sbom_has_a_deterministic_attestation_identity() {
    let generator = fs::read_to_string(repository_path("tools/New-ReleaseSbom.ps1"))
        .expect("the release SBOM generator should be readable");

    assert!(
        generator.contains("function New-DeterministicUuidV5"),
        "the SBOM identity must be deterministic across exact-source rebuilds"
    );
    assert!(
        generator.contains("'CursorPeek release SBOM'")
            && generator.contains("\"lock-sha256=$($lockHashBefore.ToLowerInvariant())\""),
        "the SBOM identity must be scoped to the product and locked release graph"
    );
    assert!(
        generator.contains("Add-Member -NotePropertyName serialNumber")
            && generator.contains("^urn:uuid:")
            && generator.contains("-5[0-9a-f]{3}-[89ab]"),
        "the generated CycloneDX document must carry the UUIDv5 serial required for attestation"
    );
}

#[test]
fn release_checksums_cover_only_the_canonical_asset_set() {
    let version = env!("CARGO_PKG_VERSION");
    let directory = repository_path(&format!(
        "target/qualification-tests/release-checksums-{}",
        std::process::id()
    ));
    if directory.exists() {
        fs::remove_dir_all(&directory).expect("the prior checksum fixture should be removable");
    }
    fs::create_dir_all(&directory).expect("the checksum fixture directory should be creatable");

    let names = [
        format!("CursorPeek-{version}.cdx.json"),
        format!("CursorPeek-{version}-windows-x64-portable.zip"),
        format!("CursorPeek-{version}-windows-x64-setup.exe"),
    ];
    for (index, name) in names.iter().enumerate() {
        fs::write(directory.join(name), format!("fixture-{index}\n"))
            .expect("the checksum fixture should be writable");
    }

    let output_path = directory.join("SHA256SUMS.txt");
    let successful = run_release_checksums(&[
        "-ArtifactsDirectory",
        directory
            .to_str()
            .expect("the fixture path should be Unicode"),
        "-OutputPath",
        output_path
            .to_str()
            .expect("the checksum output path should be Unicode"),
    ]);
    assert!(
        successful.status.success(),
        "checksum generation failed: {}",
        String::from_utf8_lossy(&successful.stderr)
    );

    let checksums =
        fs::read_to_string(&output_path).expect("the checksum manifest should be readable");
    let lines = checksums.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), names.len());
    let mut sorted_names = names;
    sorted_names.sort();
    for (line, expected_name) in lines.iter().zip(sorted_names) {
        let (digest, name) = line
            .split_once("  ")
            .expect("each checksum line should contain two spaces");
        assert_eq!(digest.len(), 64);
        assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(name, expected_name);
    }

    fs::write(directory.join("unexpected.txt"), b"unexpected\n")
        .expect("the unexpected fixture should be writable");
    let rejected = run_release_checksums(&[
        "-ArtifactsDirectory",
        directory
            .to_str()
            .expect("the fixture path should be Unicode"),
        "-OutputPath",
        output_path
            .to_str()
            .expect("the checksum output path should be Unicode"),
    ]);
    assert!(
        !rejected.status.success(),
        "an unexpected release asset was accepted"
    );

    fs::remove_dir_all(directory).expect("the checksum fixture should be removable");
}

#[test]
fn qualification_schema_examples_validate_but_do_not_pass_the_gate() {
    let mut validation_arguments = example_arguments();
    validation_arguments.push("-ValidateOnly".into());
    let validation = run_owned_arguments(&validation_arguments);
    assert!(
        validation.status.success(),
        "schema validation failed: {}",
        String::from_utf8_lossy(&validation.stderr)
    );

    let report = repository_path("target/qualification-tests/schema-example-report.md");
    let mut gate_arguments = example_arguments();
    gate_arguments.extend(["-Report".into(), report.to_string_lossy().into_owned()]);
    let gate = run_owned_arguments(&gate_arguments);
    assert!(
        !gate.status.success(),
        "parser-only examples passed the gate"
    );

    let report_text =
        fs::read_to_string(report).expect("the failed gate should still write a report");
    assert!(report_text.contains("> Gate result: **FAIL**"));
    assert!(report_text.contains("fewer than 2,000"));
    assert!(report_text.contains("does not include windows11"));
}

#[test]
fn preview_practice_refuses_an_evidence_filename_before_launch() {
    let rejected_path = repository_path("target/qualification-tests/window.tsv");
    let rejected = run_preview_measurement(&[
        "-CaseId",
        "9000001",
        "-Os",
        "windows11",
        "-Build",
        "synthetic_not_evidence",
        "-Dpi",
        "100",
        "-Layout",
        "details",
        "-Scenario",
        "example",
        "-Interaction",
        "wheel",
        "-Notes",
        "schema_guard",
        "-Results",
        rejected_path
            .to_str()
            .expect("the test path should be Unicode"),
        "-ActivationDelaySeconds",
        "0",
        "-Practice",
        "-SkipBuild",
    ]);
    assert!(
        !rejected.status.success(),
        "practice unexpectedly accepted an evidence-like filename"
    );
    assert!(
        String::from_utf8_lossy(&rejected.stderr)
            .contains("Practice observations must be written to a clearly named attempts file"),
        "unexpected practice-path rejection: {}",
        String::from_utf8_lossy(&rejected.stderr)
    );
}

#[test]
fn qualification_gate_rejects_a_repeated_point_with_a_different_case_id() {
    let temporary_directory = repository_path("target/qualification-tests");
    fs::create_dir_all(&temporary_directory)
        .expect("the qualification test directory should exist");
    let duplicate_path = temporary_directory.join("duplicate-point.tsv");

    let original = fs::read_to_string(repository_path("qualification/schema-example.resolver.tsv"))
        .expect("the resolver schema example should be readable");
    let row = original
        .lines()
        .nth(1)
        .expect("the resolver schema example should contain one row");
    let duplicate = row
        .strip_prefix("1\t")
        .map(|suffix| format!("2\t{suffix}"))
        .expect("the example row should start with case ID 1");
    fs::write(&duplicate_path, format!("{original}{duplicate}\n"))
        .expect("the duplicate resolver fixture should be written");

    let arguments = vec![
        "-ResolverResults".into(),
        duplicate_path.to_string_lossy().into_owned(),
        "-WindowEvidence".into(),
        repository_path("qualification/schema-example.window.tsv")
            .to_string_lossy()
            .into_owned(),
        "-ValidateOnly".into(),
    ];
    let output = run_owned_arguments(&arguments);
    assert!(
        !output.status.success(),
        "duplicate point evidence was accepted"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("duplicates a labeled point observation"),
        "unexpected duplicate rejection: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn qualification_gate_rejects_unobserved_click_delivery() {
    let temporary_directory = repository_path("target/qualification-tests");
    fs::create_dir_all(&temporary_directory)
        .expect("the qualification test directory should exist");
    let invalid_path = temporary_directory.join("unobserved-click.tsv");

    let original = fs::read_to_string(repository_path("qualification/schema-example.window.tsv"))
        .expect("the window schema example should be readable");
    let invalid = original.replace(
        "\ttimeout\tyes\tyes\tn/a\tyes\t",
        "\tleft_click\tyes\tyes\tn/a\tyes\t",
    );
    assert_ne!(
        invalid, original,
        "the click-delivery fixture should change"
    );
    fs::write(&invalid_path, invalid).expect("the invalid window fixture should be written");

    let arguments = vec![
        "-ResolverResults".into(),
        repository_path("qualification/schema-example.resolver.tsv")
            .to_string_lossy()
            .into_owned(),
        "-WindowEvidence".into(),
        invalid_path.to_string_lossy().into_owned(),
        "-ValidateOnly".into(),
    ];
    let output = run_owned_arguments(&arguments);
    assert!(
        !output.status.success(),
        "a click without explicit delivery evidence was accepted"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("must record click delivery as yes or no"),
        "unexpected click-delivery rejection: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn resolver_fixture_generator_is_deterministic_and_non_destructive() {
    let temporary_root = repository_path("target/qualification-tests/live-fixture");
    let inventory = repository_path("target/qualification-tests/live-fixture.inventory.tsv");
    if temporary_root.exists() {
        fs::remove_dir_all(&temporary_root)
            .expect("the prior fixture test directory should be removable");
    }
    if inventory.exists() {
        fs::remove_file(&inventory).expect("the prior fixture inventory should be removable");
    }

    let generated = run_fixture_generator(&[
        "-Destination",
        temporary_root
            .to_str()
            .expect("the fixture path should be Unicode"),
        "-FileCount",
        "8",
        "-Inventory",
        inventory
            .to_str()
            .expect("the inventory path should be Unicode"),
    ]);
    assert!(
        generated.status.success(),
        "fixture generation failed: {}",
        String::from_utf8_lossy(&generated.stderr)
    );

    let files = fs::read_dir(&temporary_root)
        .expect("the generated fixture directory should be readable")
        .collect::<Result<Vec<_>, _>>()
        .expect("fixture entries should be readable");
    assert_eq!(files.len(), 8);
    assert_eq!(
        fs::read_to_string(temporary_root.join("cursorpeek-item-0001.txt"))
            .expect("the first fixture should be readable"),
        "CursorPeek resolver fixture 0001\r\n"
    );
    assert_eq!(
        fs::read_to_string(&inventory)
            .expect("the inventory should be readable")
            .lines()
            .count(),
        9
    );

    let repeated = run_fixture_generator(&[
        "-Destination",
        temporary_root
            .to_str()
            .expect("the fixture path should be Unicode"),
        "-FileCount",
        "8",
        "-Inventory",
        repository_path("target/qualification-tests/repeated.inventory.tsv")
            .to_str()
            .expect("the repeated inventory path should be Unicode"),
    ]);
    assert!(
        !repeated.status.success(),
        "fixture generation unexpectedly overwrote a populated directory"
    );
    assert!(
        String::from_utf8_lossy(&repeated.stderr).contains("destination must be absent or empty"),
        "unexpected non-destructive guard: {}",
        String::from_utf8_lossy(&repeated.stderr)
    );

    let nested_fixture = repository_path("target/qualification-tests/nested-live-fixture");
    if nested_fixture.exists() {
        fs::remove_dir_all(&nested_fixture)
            .expect("the prior nested-inventory fixture should be removable");
    }
    let nested_inventory = nested_fixture.join("inventory.tsv");
    let rejected_inventory = run_fixture_generator(&[
        "-Destination",
        nested_fixture
            .to_str()
            .expect("the nested fixture path should be Unicode"),
        "-FileCount",
        "8",
        "-Inventory",
        nested_inventory
            .to_str()
            .expect("the nested inventory path should be Unicode"),
    ]);
    assert!(
        !rejected_inventory.status.success(),
        "fixture generation unexpectedly placed its inventory among fixture files"
    );
    assert!(
        String::from_utf8_lossy(&rejected_inventory.stderr)
            .contains("inventory must remain outside"),
        "unexpected nested-inventory guard: {}",
        String::from_utf8_lossy(&rejected_inventory.stderr)
    );
    assert!(
        !nested_fixture.exists(),
        "a rejected nested-inventory destination was modified"
    );

    fs::remove_dir_all(&temporary_root).expect("the fixture test directory should be removable");
    fs::remove_file(&inventory).expect("the fixture inventory should be removable");
}

#[test]
fn live_page_collector_validates_without_promoting_evidence() {
    let output = repository_path("target/qualification-tests/live-page-validation");
    if output.exists() {
        fs::remove_dir_all(&output)
            .expect("the prior live-page validation directory should be removable");
    }
    let valid = run_live_page_collector(&[
        "-FixturePath",
        repository_path("corpus")
            .to_str()
            .expect("the corpus path should be Unicode"),
        "-Os",
        "windows11",
        "-Build",
        "22631",
        "-Dpi",
        "175",
        "-Layout",
        "details",
        "-Scenario",
        "file_row",
        "-CaseIdStart",
        "9900000",
        "-SessionName",
        "schema-only-live-page",
        "-OutputDirectory",
        output
            .to_str()
            .expect("the validation output path should be Unicode"),
        "-ValidateOnly",
    ]);
    assert!(
        valid.status.success(),
        "live-page validation failed: {}",
        String::from_utf8_lossy(&valid.stderr)
    );
    assert!(
        !output.exists(),
        "validation-only mode unexpectedly created evidence artifacts"
    );

    let forbidden = repository_path("corpus/results/live-page-forbidden");
    let rejected = run_live_page_collector(&[
        "-FixturePath",
        repository_path("corpus")
            .to_str()
            .expect("the corpus path should be Unicode"),
        "-Os",
        "windows11",
        "-Build",
        "22631",
        "-Dpi",
        "175",
        "-Layout",
        "details",
        "-Scenario",
        "file_row",
        "-CaseIdStart",
        "9900000",
        "-SessionName",
        "schema-only-live-page",
        "-OutputDirectory",
        forbidden
            .to_str()
            .expect("the forbidden output path should be Unicode"),
        "-ValidateOnly",
    ]);
    assert!(
        !rejected.status.success(),
        "live collection unexpectedly accepted an evidence destination"
    );
    assert!(
        String::from_utf8_lossy(&rejected.stderr)
            .contains("ignored review output, not accepted evidence paths"),
        "unexpected evidence-path guard: {}",
        String::from_utf8_lossy(&rejected.stderr)
    );
    assert!(
        !forbidden.exists(),
        "a rejected evidence destination was modified"
    );

    let invalid_settle_output =
        repository_path("target/qualification-tests/live-page-invalid-settle");
    let invalid_settle = run_live_page_collector(&[
        "-FixturePath",
        repository_path("corpus")
            .to_str()
            .expect("the corpus path should be Unicode"),
        "-Os",
        "windows11",
        "-Build",
        "22631",
        "-Dpi",
        "175",
        "-Layout",
        "details",
        "-Scenario",
        "file_row",
        "-CaseIdStart",
        "9900000",
        "-SessionName",
        "schema-only-live-page",
        "-SelectionSettleMilliseconds",
        "99",
        "-OutputDirectory",
        invalid_settle_output
            .to_str()
            .expect("the invalid-settle output path should be Unicode"),
        "-ValidateOnly",
    ]);
    assert!(
        !invalid_settle.status.success(),
        "live collection unexpectedly accepted an unsafe selection-settling interval"
    );
    assert!(
        !invalid_settle_output.exists(),
        "an invalid selection-settling interval created output"
    );

    let grid_output = repository_path("target/qualification-tests/live-page-grid-validation");
    let valid_grid = run_live_page_collector(&[
        "-FixturePath",
        repository_path("corpus")
            .to_str()
            .expect("the corpus path should be Unicode"),
        "-Os",
        "windows11",
        "-Build",
        "22631",
        "-Dpi",
        "175",
        "-Layout",
        "details",
        "-Scenario",
        "file_row",
        "-CaseIdStart",
        "9900000",
        "-SessionName",
        "schema-only-live-page-grid",
        "-PointProfile",
        "item_grid",
        "-GridSpacingPixels",
        "16",
        "-GridRows",
        "2",
        "-GridEdgeInsetPixels",
        "32",
        "-MaxGridPointsPerItem",
        "128",
        "-MaxCases",
        "4096",
        "-OutputDirectory",
        grid_output
            .to_str()
            .expect("the grid validation output path should be Unicode"),
        "-ValidateOnly",
    ]);
    assert!(
        valid_grid.status.success(),
        "bounded live-page grid validation failed: {}",
        String::from_utf8_lossy(&valid_grid.stderr)
    );
    assert!(
        !grid_output.exists(),
        "grid validation-only mode unexpectedly created evidence artifacts"
    );

    let invalid_grid_output = repository_path("target/qualification-tests/live-page-invalid-grid");
    let invalid_grid = run_live_page_collector(&[
        "-FixturePath",
        repository_path("corpus")
            .to_str()
            .expect("the corpus path should be Unicode"),
        "-Os",
        "windows11",
        "-Build",
        "22631",
        "-Dpi",
        "175",
        "-Layout",
        "details",
        "-Scenario",
        "file_row",
        "-CaseIdStart",
        "9900000",
        "-SessionName",
        "schema-only-live-page-invalid-grid",
        "-PointProfile",
        "item_grid",
        "-GridSpacingPixels",
        "7",
        "-OutputDirectory",
        invalid_grid_output
            .to_str()
            .expect("the invalid-grid output path should be Unicode"),
        "-ValidateOnly",
    ]);
    assert!(
        !invalid_grid.status.success(),
        "live collection unexpectedly accepted an unsafe grid spacing"
    );
    assert!(
        !invalid_grid_output.exists(),
        "an invalid grid created output"
    );

    let misplaced_grid_output =
        repository_path("target/qualification-tests/live-page-misplaced-grid");
    let misplaced_grid = run_live_page_collector(&[
        "-FixturePath",
        repository_path("corpus")
            .to_str()
            .expect("the corpus path should be Unicode"),
        "-Os",
        "windows11",
        "-Build",
        "22631",
        "-Dpi",
        "175",
        "-Layout",
        "details",
        "-Scenario",
        "file_row",
        "-CaseIdStart",
        "9900000",
        "-SessionName",
        "schema-only-live-page-misplaced-grid",
        "-GridRows",
        "2",
        "-OutputDirectory",
        misplaced_grid_output
            .to_str()
            .expect("the misplaced-grid output path should be Unicode"),
        "-ValidateOnly",
    ]);
    assert!(
        !misplaced_grid.status.success(),
        "non-grid collection unexpectedly accepted grid-only parameters"
    );
    assert!(
        !misplaced_grid_output.exists(),
        "misplaced grid parameters created output"
    );
}

#[test]
fn qualification_gate_pass_path_aggregates_all_required_metrics() {
    let temporary_directory = repository_path("target/qualification-tests/synthetic-pass");
    if temporary_directory.exists() {
        fs::remove_dir_all(&temporary_directory)
            .expect("the prior synthetic qualification directory should be removable");
    }
    fs::create_dir_all(&temporary_directory)
        .expect("the synthetic qualification directory should exist");

    let scenario_text = fs::read_to_string(repository_path("corpus/scenarios.tsv"))
        .expect("the scenario matrix should be readable");
    let scenarios = scenario_text
        .lines()
        .skip(1)
        .map(|line| {
            let fields = line.split('\t').collect::<Vec<_>>();
            assert_eq!(
                fields.len(),
                4,
                "the scenario matrix should have four fields"
            );
            (
                fields[0].to_owned(),
                fields[1].to_owned(),
                fields[2].to_owned(),
            )
        })
        .collect::<Vec<_>>();
    assert!(!scenarios.is_empty());

    let resolver_header =
        fs::read_to_string(repository_path("qualification/schema-example.resolver.tsv"))
            .expect("the resolver schema example should be readable")
            .lines()
            .next()
            .expect("the resolver schema should have a header")
            .to_owned();
    let dpi_values = ["100", "125", "150", "175", "200"];
    for os in ["windows10", "windows11"] {
        let path = temporary_directory.join(format!("{os}.results.tsv"));
        let mut contents = String::with_capacity(256_000);
        contents.push_str(&resolver_header);
        contents.push('\n');

        for index in 0..1_000_usize {
            let (scenario, expectation, matrix_layout) = &scenarios[index % scenarios.len()];
            let layout = if matrix_layout == "all" {
                "details"
            } else {
                matrix_layout
            };
            let expected_path = format!(r"C:\synthetic_not_evidence\file_{index}.txt");
            let (expected, status, actual, reason, verdict) = if expectation == "resolve" {
                (
                    expected_path.as_str(),
                    "resolved",
                    expected_path.as_str(),
                    "shell.resolved",
                    "correct_positive",
                )
            } else {
                (
                    "",
                    "unsupported",
                    "",
                    "uia.unsupported_location",
                    "correct_fail_closed",
                )
            };
            let fields = [
                (index + 1).to_string(),
                os.to_owned(),
                "synthetic_not_evidence".to_owned(),
                dpi_values[index % dpi_values.len()].to_owned(),
                layout.to_owned(),
                scenario.to_owned(),
                index.to_string(),
                (index + 10_000).to_string(),
                expectation.to_owned(),
                expected.to_owned(),
                status.to_owned(),
                actual.to_owned(),
                "10".to_owned(),
                reason.to_owned(),
                "0".to_owned(),
                "0".to_owned(),
                verdict.to_owned(),
            ];
            contents.push_str(&fields.join("\t"));
            contents.push('\n');
        }
        fs::write(&path, contents).expect("the synthetic resolver results should be written");
    }

    let window_header =
        fs::read_to_string(repository_path("qualification/schema-example.window.tsv"))
            .expect("the window schema example should be readable")
            .lines()
            .next()
            .expect("the window schema should have a header")
            .to_owned();
    let window_scenarios = [
        "center",
        "work_area_top_left",
        "work_area_top_right",
        "work_area_bottom_left",
        "work_area_bottom_right",
        "negative_origin_monitor",
        "mixed_dpi_transition",
        "explorer_restart",
    ];
    let interactions = ["timeout", "move", "wheel", "left_click", "right_click"];
    let window_path = temporary_directory.join("window.tsv");
    let mut window_contents = format!("{window_header}\n");
    let mut case_id = 1_u64;
    for os in ["windows10", "windows11"] {
        for (index, scenario) in window_scenarios.iter().enumerate() {
            let interaction = interactions[index % interactions.len()];
            let click_delivered = if matches!(interaction, "left_click" | "right_click") {
                "yes"
            } else {
                "n/a"
            };
            let ui_thread_max_us = if *scenario == "mixed_dpi_transition" {
                "40004"
            } else {
                "10"
            };
            let fields = [
                case_id.to_string(),
                os.to_owned(),
                "synthetic_not_evidence".to_owned(),
                dpi_values[index % dpi_values.len()].to_owned(),
                "details".to_owned(),
                (*scenario).to_owned(),
                interaction.to_owned(),
                "yes".to_owned(),
                "yes".to_owned(),
                click_delivered.to_owned(),
                "yes".to_owned(),
                "yes".to_owned(),
                "yes".to_owned(),
                ui_thread_max_us.to_owned(),
                "Synthetic parser pass-path test only; not evidence.".to_owned(),
            ];
            window_contents.push_str(&fields.join("\t"));
            window_contents.push('\n');
            case_id += 1;
        }
    }
    fs::write(&window_path, &window_contents)
        .expect("the synthetic window evidence should be written");

    let report_path = temporary_directory.join("report.md");
    let arguments = vec![
        "-ResolverResultsDirectory".into(),
        temporary_directory.to_string_lossy().into_owned(),
        "-WindowEvidence".into(),
        window_path.to_string_lossy().into_owned(),
        "-Report".into(),
        report_path.to_string_lossy().into_owned(),
    ];
    let output = run_owned_arguments(&arguments);
    assert!(
        output.status.success(),
        "synthetic pass-path aggregation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let report =
        fs::read_to_string(&report_path).expect("the synthetic pass report should be readable");
    assert!(report.contains("> Gate result: **PASS**"));
    assert!(report.contains("| Independent labeled rows | 2000 |"));
    assert!(report.contains("| Positive coverage | 100.000% |"));
    assert!(report.contains("| Wrong paths | 0 |"));
    assert!(report.contains("| Latency p95 | 10 us |"));
    assert!(report.contains("Failed focus/click/placement/initial-task-bound rows: 0."));
    assert!(report.contains("risk-based rather than a Cartesian product"));
    assert!(report.contains("16,000 us steady-state UI-thread ceiling"));
    assert!(report.contains("| windows11 | 8 | 40004 |"));

    let incomplete_window_path = temporary_directory.join("window-without-topology.tsv");
    let incomplete_window_contents = window_contents
        .lines()
        .filter(|line| {
            !line.contains("\tnegative_origin_monitor\t")
                && !line.contains("\tmixed_dpi_transition\t")
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(&incomplete_window_path, incomplete_window_contents)
        .expect("the topology-incomplete window evidence should be written");

    let incomplete_release_report = temporary_directory.join("incomplete-release-report.md");
    let incomplete_release_arguments = vec![
        "-ResolverResultsDirectory".into(),
        temporary_directory.to_string_lossy().into_owned(),
        "-WindowEvidence".into(),
        incomplete_window_path.to_string_lossy().into_owned(),
        "-Report".into(),
        incomplete_release_report.to_string_lossy().into_owned(),
    ];
    let incomplete_release = run_owned_arguments(&incomplete_release_arguments);
    assert!(
        !incomplete_release.status.success(),
        "the release gate unexpectedly accepted window evidence without topology cases"
    );
    let incomplete_release_text = fs::read_to_string(&incomplete_release_report)
        .expect("the incomplete release report should be readable");
    assert!(incomplete_release_text.contains("> Gate result: **FAIL**"));
    assert!(incomplete_release_text.contains("`aggregate scenario=negative_origin_monitor`"));
    assert!(incomplete_release_text.contains("`aggregate scenario=mixed_dpi_transition`"));

    let over_budget_window_path = temporary_directory.join("window-over-budget.tsv");
    let over_budget_window_contents = window_contents.replacen("\t40004\t", "\t50001\t", 1);
    assert_ne!(
        over_budget_window_contents, window_contents,
        "the synthetic matrix should contain a realistic create/show observation"
    );
    fs::write(&over_budget_window_path, over_budget_window_contents)
        .expect("the over-budget window evidence should be written");
    let over_budget_report = temporary_directory.join("over-budget-report.md");
    let over_budget_arguments = vec![
        "-ResolverResultsDirectory".into(),
        temporary_directory.to_string_lossy().into_owned(),
        "-WindowEvidence".into(),
        over_budget_window_path.to_string_lossy().into_owned(),
        "-Report".into(),
        over_budget_report.to_string_lossy().into_owned(),
    ];
    let over_budget = run_owned_arguments(&over_budget_arguments);
    assert!(
        !over_budget.status.success(),
        "the release gate accepted a preview create/show task above 50 ms"
    );
    let over_budget_text =
        fs::read_to_string(&over_budget_report).expect("the over-budget report should be readable");
    assert!(over_budget_text.contains("Failed focus/click/placement/initial-task-bound rows: 1."));

    fs::remove_dir_all(&temporary_directory)
        .expect("the synthetic qualification directory should be removable");
}
