// P8: optional preferred_min_size hint in [provides] parses cleanly,
// defaults to None when absent, and surfaces malformed shapes as a
// parse error rather than silently dropping data.

use ainb_plugin_protocol::manifest::Manifest;

#[test]
fn manifest_without_preferred_min_size_defaults_to_none() {
    let toml_src = r#"
[plugin]
name = "demo"
version = "0.1.0"
abi_version = 2

[provides]
screens = ["demo"]
"#;
    let m: Manifest = toml::from_str(toml_src).expect("parse");
    assert!(m.provides.preferred_min_size.is_none());
}

#[test]
fn manifest_with_preferred_min_size_parses_to_some_pair() {
    let toml_src = r#"
[plugin]
name = "demo"
version = "0.1.0"
abi_version = 2

[provides]
screens = ["demo"]
preferred_min_size = [40, 12]
"#;
    let m: Manifest = toml::from_str(toml_src).expect("parse");
    assert_eq!(m.provides.preferred_min_size, Some([40, 12]));
}

#[test]
fn manifest_with_malformed_preferred_min_size_fails_parse() {
    // single-element array doesn't fit [u16; 2]
    let toml_src = r#"
[plugin]
name = "demo"
version = "0.1.0"
abi_version = 2

[provides]
preferred_min_size = [40]
"#;
    let r: Result<Manifest, _> = toml::from_str(toml_src);
    assert!(r.is_err(), "[40] must not parse as a [u16; 2]");
}

#[test]
fn manifest_serialise_roundtrips_preferred_min_size() {
    let toml_src = r#"
[plugin]
name = "demo"
version = "0.1.0"
abi_version = 2

[provides]
preferred_min_size = [50, 15]
"#;
    let m: Manifest = toml::from_str(toml_src).expect("parse");
    let re_toml = toml::to_string(&m).expect("serialise");
    assert!(
        re_toml.contains("preferred_min_size"),
        "re-serialised toml must keep the field, got:\n{re_toml}"
    );
    let back: Manifest = toml::from_str(&re_toml).expect("re-parse");
    assert_eq!(back.provides.preferred_min_size, Some([50, 15]));
}

#[test]
fn manifest_with_min_size_at_zero_parses_but_no_special_treatment() {
    // 0×0 is a degenerate hint; host clamps via global default later
    let toml_src = r#"
[plugin]
name = "demo"
version = "0.1.0"
abi_version = 2

[provides]
preferred_min_size = [0, 0]
"#;
    let m: Manifest = toml::from_str(toml_src).expect("parse");
    assert_eq!(m.provides.preferred_min_size, Some([0, 0]));
}
