use std::collections::HashMap;
use std::fmt::Display;
use std::ops::Deref;
use std::path::PathBuf;

use anyhow::Result;
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use thiserror::Error;
use url::Url;

use crate::config::{self, read_config, read_manifest, ConfigLocalHook, ConfigLocalRepo, ConfigRemoteHook, ConfigRemoteRepo, ConfigRepo, ConfigWire, ManifestHook, CONFIG_FILE, MANIFEST_FILE};
use crate::fs::CWD;
use crate::store::Store;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Failed to parse URL: {0}")]
    InvalidUrl(#[from] url::ParseError),
    #[error("Failed to read config file: {0}")]
    ReadConfig(#[from] config::Error),
    #[error("Failed to initialize repo: {0}")]
    InitRepo(#[from] anyhow::Error),
    #[error("Hook not found: {hook} in repo {repo}")]
    HookNotFound { hook: String, repo: String },
}

#[derive(Debug)]
pub struct RemoteRepo {
    /// Path to the stored repo.
    path: PathBuf,
    url: Url,
    rev: String,
    hooks: HashMap<String, ManifestHook>,
}

#[derive(Debug)]
pub struct LocalRepo {
    path: PathBuf,
    hooks: HashMap<String, ConfigLocalHook>,
}

#[derive(Debug)]
pub enum Repo {
    Remote(RemoteRepo),
    Local(LocalRepo),
    Meta,
}

impl Repo {
    pub fn remote(url: &str, rev: &str, path: &str) -> Result<Self> {
        let url = Url::parse(&url).map_err(Error::InvalidUrl)?;

        let path = PathBuf::from(path);
        let path = path.join(MANIFEST_FILE);
        let manifest = read_manifest(&path)?;
        let hooks = manifest
            .hooks
            .into_iter()
            .map(|hook| (hook.id.clone(), hook))
            .collect();

        Ok(Self::Remote(RemoteRepo {
            path,
            url,
            rev: rev.to_string(),
            hooks,
        }))
    }

    pub fn local(hooks: Vec<ConfigLocalHook>, path: &str) -> Result<Self> {
        let hooks = hooks
            .into_iter()
            .map(|hook| (hook.id.clone(), hook))
            .collect();

        let path = PathBuf::from(path);
        Ok(Self::Local(LocalRepo { path, hooks }))
    }

    pub fn meta() -> Self {
        todo!()
    }

    pub fn get_hook(&self, id: &str) -> Option<&ManifestHook> {
        match self {
            Repo::Remote(repo) => repo.hooks.get(id),
            Repo::Local(repo) => repo.hooks.get(id),
            Repo::Meta => None,
        }
    }
}

impl Display for Repo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Repo::Remote(repo) => write!(f, "{}@{}", repo.url, repo.rev),
            Repo::Local(_) => write!(f, "local"),
            Repo::Meta => write!(f, "meta"),
        }
    }
}

pub struct Project {
    root: PathBuf,
    config: ConfigWire,
}

impl Project {
    pub fn from_directory(root: PathBuf, config: Option<PathBuf>) -> Result<Self> {
        let config_path = config.unwrap_or_else(|| root.join(CONFIG_FILE));
        let config = read_config(&config_path).map_err(Error::ReadConfig)?;
        Ok(Self { root, config })
    }

    pub fn current(config: Option<PathBuf>) -> Result<Self> {
        Self::from_directory(CWD.clone(), config)
    }

    // pub fn repos(&self, store: &Store) -> Result<Vec<Repo>> {
    //     // TODO: init in parallel
    //     self.config
    //         .repos
    //         .iter()
    //         .map(|repo| store.clone_repo(repo, None))
    //         .collect::<Result<_>>()
    // }

    pub async fn hooks(&self, store: &Store) -> Result<Vec<Hook>> {
        let mut hooks = Vec::new();

        // TODO: progress bar
        // Prepare repos.
        let mut tasks = FuturesUnordered::new();
        for repo_config in &self.config.repos {
            tasks.push(async { (repo_config, store.prepare_repo(repo_config, None).await) });
        }

        let mut hook_tasks = FuturesUnordered::new();

        while let Some((repo_config, repo)) = tasks.next().await {
            let repo = repo?;
            match repo_config {
                ConfigRepo::Remote(ConfigRemoteRepo { hooks: remote_hooks, .. }) => {
                    for hook_config in remote_hooks {
                        // Check hook id is valid.
                        let Some(manifest_hook) = repo.get_hook(&hook_config.id) else {
                            return Err(Error::HookNotFound {
                                hook: hook_config.id.clone(),
                                repo: repo.to_string(),
                            })?;
                        };

                        let mut hook = Hook::from(manifest_hook.clone());
                        hook.update(hook_config.clone());
                        hook.fill(&self.config);

                        if let Some(deps) = &hook.additional_dependencies {
                            hook_tasks.push(store.prepare_repo(repo_config, Some(deps.clone())));
                        }

                        hooks.push(hook);
                    }
                }
                ConfigRepo::Local(ConfigLocalRepo {hooks: local_hooks,..}) => {
                    for hook_config in local_hooks {
                        let mut hook = Hook::from(hook_config.clone());
                        hook.fill(&self.config);

                        if let Some(deps) = &hook.additional_dependencies {
                            hook_tasks.push(store.prepare_repo(repo_config, Some(deps.clone())));
                        }

                        hooks.push(hook);
                    }
                }
                ConfigRepo::Meta(_) => {}
            }
        }

        // Prepare hooks with `additional_dependencies` (they need separate repos).
        hook_tasks.collect().await?;

        Ok(hooks)
    }
}

#[derive(Debug)]
pub struct Hook(ManifestHook);

impl From<ManifestHook> for Hook {
    fn from(hook: ManifestHook) -> Self {
        Self(hook)
    }
}

impl Deref for Hook {
    type Target = ManifestHook;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Hook {
    pub fn update(&mut self, repo_hook: ConfigRemoteHook) {
        self.0.alias = repo_hook.alias;

        if let Some(name) = repo_hook.name {
            self.0.name = name;
        }
        if repo_hook.language_version.is_some() {
            self.0.language_version = repo_hook.language_version;
        }
        if repo_hook.files.is_some() {
            self.0.files = repo_hook.files;
        }
        if repo_hook.exclude.is_some() {
            self.0.exclude = repo_hook.exclude;
        }
        if repo_hook.types.is_some() {
            self.0.types = repo_hook.types;
        }
        if repo_hook.types_or.is_some() {
            self.0.types_or = repo_hook.types_or;
        }
        if repo_hook.exclude_types.is_some() {
            self.0.exclude_types = repo_hook.exclude_types;
        }
        if repo_hook.args.is_some() {
            self.0.args = repo_hook.args;
        }
        if repo_hook.stages.is_some() {
            self.0.stages = repo_hook.stages;
        }
        if repo_hook.additional_dependencies.is_some() {
            self.0.additional_dependencies = repo_hook.additional_dependencies;
        }
        if repo_hook.always_run.is_some() {
            self.0.always_run = repo_hook.always_run;
        }
        if repo_hook.verbose.is_some() {
            self.0.verbose = repo_hook.verbose;
        }
        if repo_hook.log_file.is_some() {
            self.0.log_file = repo_hook.log_file;
        }
    }

    pub fn fill(&mut self, config: &ConfigWire) {
        let language = self.0.language;
        if self.0.language_version.is_none() {
            self.0.language_version = config
                .default_language_version
                .as_ref()
                .and_then(|v| v.get(&language).cloned())
        }
        if self.0.language_version.is_none() {
            self.0.language_version = Some(language.default_version());
        }

        if self.0.stages.is_none() {
            self.0.stages = config.default_stages.clone();
        }

        // TODO: check ENVIRONMENT_DIR with language_version and additional_dependencies
    }
}
