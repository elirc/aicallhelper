use app_core::llm::LlmProviderKind;
use app_core::store::{CallProfilePatch, SettingsPatch, SettingsStore, DEFAULT_PROFILE_ID};

#[test]
fn local_mode_round_trips_without_requiring_or_clearing_cloud_keys() {
    let dir = tempfile::tempdir().unwrap();
    let store = SettingsStore::load_from(dir.path());
    store
        .apply_patch(SettingsPatch {
            anthropic_key: Some("existing-answer-test-key".into()),
            deepgram_key: Some("existing-speech-test-key".into()),
            ..Default::default()
        })
        .unwrap();
    store
        .apply_patch(SettingsPatch {
            llm_provider: Some(LlmProviderKind::Local),
            profiles: Some(vec![CallProfilePatch {
                id: DEFAULT_PROFILE_ID.into(),
                name: "Default".into(),
                resume: "My experience".into(),
                ..Default::default()
            }]),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(store.get().llm_provider, LlmProviderKind::Local);
    assert_eq!(store.get().active_llm_key(), None);
    let restored = SettingsStore::load_from(dir.path());
    assert_eq!(restored.get().llm_provider, LlmProviderKind::Local);
    assert_eq!(restored.get().active_profile().resume, "My experience");
    assert_eq!(
        restored.get().anthropic_key.as_deref(),
        Some("existing-answer-test-key")
    );
    assert_eq!(
        restored.get().deepgram_key.as_deref(),
        Some("existing-speech-test-key")
    );
}
