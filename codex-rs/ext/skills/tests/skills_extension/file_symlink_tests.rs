use pretty_assertions::assert_eq;

use super::*;

#[tokio::test]
async fn file_symlink_catalog_resolves_target_and_honors_disabled_aliases() -> TestResult {
    let root = tempfile::tempdir()?;
    let home = AbsolutePathBuf::try_from(std::fs::canonicalize(root.path())?)?;
    let skills_root = home.join("skills");
    let source = home.join("shared/SKILL.md");
    let alias = skills_root.join("demo/SKILL.md");
    std::fs::create_dir_all(source.parent().ok_or("source parent")?)?;
    std::fs::create_dir_all(alias.parent().ok_or("alias parent")?)?;
    std::fs::write(&source, DEMO_SKILL_CONTENTS)?;
    std::os::unix::fs::symlink(&source, &alias)?;

    for disabled_path in [None, Some(&alias), Some(&source)] {
        let mut settings = toml::toml! {
            [skills.bundled]
            enabled = false
        };
        if let Some(path) = disabled_path {
            settings["skills"]
                .as_table_mut()
                .ok_or("skills table")?
                .insert(
                    "config".to_string(),
                    toml::Value::Array(vec![toml::Value::Table(toml::toml! {
                        path = (path.to_string_lossy().into_owned())
                        enabled = false
                    })]),
                );
        }
        let stack = ConfigLayerStack::new(
            vec![ConfigLayerEntry::new(
                ConfigLayerSource::SessionFlags,
                settings.into(),
            )],
            Default::default(),
            ConfigRequirementsToml::default(),
        )?;
        let service = HostSkillsService::new(home.clone(), /*bundled_skills_enabled*/ false);
        service.set_extra_roots(vec![skills_root.clone()]);
        let snapshot = service
            .snapshot_for_config(
                &HostSkillsLoadInput::new(home.clone(), Vec::new(), stack),
                Some(Arc::clone(&LOCAL_FS)),
            )
            .await;
        assert_eq!(snapshot.outcome().errors, Vec::new());
        let skills = &snapshot.outcome().skills;
        assert_eq!(skills.len(), 1);
        assert_eq!(&skills[0].path_to_skills_md, &source);
        assert_eq!(
            snapshot.outcome().is_skill_enabled(&skills[0]),
            disabled_path.is_none()
        );

        let mut builder = ExtensionRegistryBuilder::new();
        install(&mut builder, skills_extension_config);
        let registry = builder.build();
        let session_store = ExtensionData::new("session");
        let thread_store = ExtensionData::new("thread");
        let mut config = default_config();
        config.bundled_skills_enabled = false;
        registry.thread_lifecycle_contributors()[0]
            .on_thread_start(ThreadStartInput {
                config: &config,
                session_source: &SessionSource::Cli,
                persistent_thread_state_available: true,
                environments: &[],
                mcp_resource_client: None,
                extension_metrics: None,
                session_store: &session_store,
                thread_store: &thread_store,
            })
            .await;
        thread_store.insert(catalog_model_info());
        let turn_store = ExtensionData::new("turn");
        turn_store.insert(snapshot);
        let fragments = registry.turn_input_contributors()[0]
            .contribute(
                TurnInputContext {
                    turn_id: "turn".to_string(),
                    user_input: vec![UserInput::Text {
                        text: "$demo".to_string(),
                        text_elements: Vec::new(),
                    }],
                    environments: Vec::new(),
                },
                /*extension_metrics*/ None,
                &session_store,
                &thread_store,
                &turn_store,
            )
            .await;
        let catalog = fragments
            .iter()
            .map(|fragment| fragment.render())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            catalog.contains("- demo:"),
            disabled_path.is_none(),
            "{catalog}"
        );
        assert_eq!(
            catalog.contains("Use the demo skill."),
            disabled_path.is_none(),
            "{catalog}"
        );
        if disabled_path.is_none() {
            assert!(catalog.contains("demo/SKILL.md"), "{catalog}");
        }
    }
    Ok(())
}
