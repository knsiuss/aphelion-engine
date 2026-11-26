use aphelion_cli::ide_activator::{
    activate_integration_in, deactivate_integration_in, inspect_integrations_in,
};
use aphelion_cli::ui_state::detect_client_tag;
use hyper::HeaderMap;
use std::fs;

#[test]
fn dynamic_client_tag_detects_any_harness_without_hardcoded_list() {
    let mut h_title = HeaderMap::new();
    h_title.insert("x-title", "Mistral Vibe Custom".parse().unwrap());
    assert_eq!(detect_client_tag(&h_title), "Mistral Vibe Custom");

    for (ua, expected) in [
        ("mistral-vibe/0.4.1 (win32)", "mistral-vibe"),
        ("codex-cli/1.2.0", "codex-cli"),
        ("zed/0.180.0", "zed"),
        ("curl/8.4.0", "CLI"),
        ("python-httpx/0.27.2", "SDK"),
    ] {
        let mut h = HeaderMap::new();
        h.insert("user-agent", ua.parse().unwrap());
        assert_eq!(detect_client_tag(&h), expected, "ua={ua}");
    }
}

#[test]
fn universal_activator_supports_toml_yaml_and_json_harnesses() {
    let tmp_home = std::env::temp_dir().join(format!("aphelion_univ_harness_{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp_home);

    // Seed Mistral Vibe (.toml), Codex CLI (.toml), OpenCode (.json), Aider (.yml), Claude (.json)
    fs::create_dir_all(tmp_home.join(".vibe")).unwrap();
    fs::write(tmp_home.join(".vibe/config.toml"), "active_model = \"devstral\"\n").unwrap();

    fs::create_dir_all(tmp_home.join(".codex")).unwrap();
    fs::write(tmp_home.join(".codex/config.toml"), "model = \"o4-mini\"\n").unwrap();

    fs::create_dir_all(tmp_home.join(".config/opencode")).unwrap();
    fs::write(tmp_home.join(".config/opencode/opencode.json"), r#"{"theme":"dark"}"#).unwrap();

    fs::write(tmp_home.join(".aider.conf.yml"), "model: deepseek\n").unwrap();

    fs::create_dir_all(tmp_home.join(".claude")).unwrap();
    fs::write(tmp_home.join(".claude/settings.json"), r#"{"enabledPlugins":{"p":true}}"#).unwrap();

    let base = "http://127.0.0.1:18789";
    let items = inspect_integrations_in(&tmp_home, base);
    let detected_ids: Vec<&str> = items.iter().filter(|i| i.detected).map(|i| i.id.as_str()).collect();
    for expected in ["claude_code", "mistral_vibe", "codex_cli", "opencode", "aider"] {
        assert!(detected_ids.contains(&expected), "missing detected harness {expected}");
    }

    // Activate & deactivate Mistral Vibe (TOML) preserving unrelated keys
    let mv_on = activate_integration_in(&tmp_home, "mistral_vibe", base).unwrap();
    assert!(mv_on.active);
    let toml_on = fs::read_to_string(tmp_home.join(".vibe/config.toml")).unwrap();
    assert!(toml_on.contains("api_base = \"http://127.0.0.1:18789/v1\""));
    assert!(toml_on.contains("active_model = \"devstral\""));
    let mv_off = deactivate_integration_in(&tmp_home, "mistral_vibe", base).unwrap();
    assert!(!mv_off.active);
    let toml_restored = fs::read_to_string(tmp_home.join(".vibe/config.toml")).unwrap();
    assert_eq!(toml_restored, "active_model = \"devstral\"\n");

    // Activate & restore Codex (TOML), OpenCode (JSON), and Aider (YAML)
    assert!(activate_integration_in(&tmp_home, "codex_cli", base).unwrap().active);
    assert!(fs::read_to_string(tmp_home.join(".codex/config.toml")).unwrap().contains("base_url = \"http://127.0.0.1:18789/v1\""));
    assert!(!deactivate_integration_in(&tmp_home, "codex_cli", base).unwrap().active);

    assert!(activate_integration_in(&tmp_home, "opencode", base).unwrap().active);
    let oc_json = fs::read_to_string(tmp_home.join(".config/opencode/opencode.json")).unwrap();
    assert!(oc_json.contains("http://127.0.0.1:18789/v1"));
    assert!(oc_json.contains("\"theme\": \"dark\""));
    assert!(!deactivate_integration_in(&tmp_home, "opencode", base).unwrap().active);

    assert!(activate_integration_in(&tmp_home, "aider", base).unwrap().active);
    assert!(fs::read_to_string(tmp_home.join(".aider.conf.yml")).unwrap().contains("openai-api-base: http://127.0.0.1:18789/v1"));
    assert!(!deactivate_integration_in(&tmp_home, "aider", base).unwrap().active);

    let _ = fs::remove_dir_all(&tmp_home);
}
