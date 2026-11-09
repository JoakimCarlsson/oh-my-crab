//! Turning text into vectors whose closeness follows its meaning.
//!
//! The model is [`MODEL_REPO`], a static embedding model trained for code: it
//! looks up a vector for each token and averages them, so it needs no GPU and
//! embeds thousands of chunks a second on one core. Its files are downloaded
//! once, from a pinned revision, into a folder of crab's cache, and loaded the
//! first time something asks for them. [`Embedder`] is that lazily loaded
//! model, shared by every caller of the process.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use model2vec_rs::model::StaticModel;
use tokio::io::AsyncWriteExt;

use crate::IndexError;

/// The Hugging Face repository of the embedding model.
pub const MODEL_REPO: &str = "minishlab/potion-code-16M";

/// The revision of [`MODEL_REPO`] that is downloaded, so that every index is built with the same vectors.
const MODEL_REVISION: &str = "1b0ff71095656b23306542bbad34a09109673720";

/// The files of the model.
const MODEL_FILES: &[&str] = &["config.json", "tokenizer.json", "model.safetensors"];

/// The most tokens of one text that are embedded.
const MAX_TOKENS: usize = 512;

/// How many texts are embedded at once.
const BATCH: usize = 256;

/// How long a failure to download or load the model is remembered before it is tried again.
const RETRY_AFTER: Duration = Duration::from_secs(600);

/// Something that embeds text.
pub trait Embed: Send + Sync {
    /// The name of the model, which an index records so that it never mixes vectors of two models.
    fn model(&self) -> &str;

    /// How many numbers each vector holds.
    fn dimensions(&self) -> usize;

    /// The vector of each of `texts`, in order, each of length one.
    fn embed(&self, texts: &[String]) -> Vec<Vec<f32>>;
}

/// The model2vec model loaded from a folder.
pub struct StaticEmbedder {
    /// The model.
    model: StaticModel,
    /// What [`Embed::model`] says.
    name: String,
}

impl StaticEmbedder {
    /// Loads the model whose files are in `folder`.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::Io`] when a file cannot be read and [`IndexError::Model`] when the files do not make a model.
    pub fn load(folder: &Path) -> Result<Self, IndexError> {
        let read = |name: &str| {
            let path = folder.join(name);
            std::fs::read(&path).map_err(|error| IndexError::io(path, error))
        };
        let model = StaticModel::from_bytes(
            read("tokenizer.json")?,
            read("model.safetensors")?,
            read("config.json")?,
            Some(true),
        )
        .map_err(|error| IndexError::Model(error.to_string()))?;
        Ok(Self {
            model,
            name: format!("{MODEL_REPO}@{MODEL_REVISION}"),
        })
    }
}

impl Embed for StaticEmbedder {
    /// The repository and revision of the model.
    fn model(&self) -> &str {
        &self.name
    }

    /// The width of the model's vectors.
    fn dimensions(&self) -> usize {
        self.model.dim()
    }

    /// Embeds `texts` in batches, each cut to [`MAX_TOKENS`] tokens.
    fn embed(&self, texts: &[String]) -> Vec<Vec<f32>> {
        self.model.encode_with_args(texts, Some(MAX_TOKENS), BATCH)
    }
}

/// How far the model of an [`Embedder`] got.
enum State {
    /// Not loaded yet.
    Unloaded,
    /// Loaded and ready.
    Ready(Arc<dyn Embed>),
    /// Downloading or loading failed at the time given, for the reason given.
    Failed(Instant, String),
}

/// The embedding model, downloaded and loaded the first time it is needed and then kept.
pub struct Embedder {
    /// The folder the model's files are kept in.
    folder: PathBuf,
    /// Where the files are downloaded from: the address of the revision, to which each file's name is added.
    source: String,
    /// The model, once loaded.
    state: Mutex<State>,
    /// Held while the files are downloaded, so that two callers never download them at once.
    fetching: tokio::sync::Mutex<()>,
}

impl Embedder {
    /// The model kept in `folder`, downloaded from Hugging Face when it is not there.
    pub fn new(folder: impl Into<PathBuf>) -> Self {
        Self::from_source(
            folder,
            format!("https://huggingface.co/{MODEL_REPO}/resolve/{MODEL_REVISION}"),
        )
    }

    /// The model kept in `folder`, downloaded from `source`, the address each file's name is added to.
    pub fn from_source(folder: impl Into<PathBuf>, source: impl Into<String>) -> Self {
        Self {
            folder: folder.into(),
            source: source.into(),
            state: Mutex::new(State::Unloaded),
            fetching: tokio::sync::Mutex::new(()),
        }
    }

    /// An embedder that already holds `model` and never downloads anything.
    pub fn with_model(model: Arc<dyn Embed>) -> Self {
        let embedder = Self::from_source(PathBuf::new(), String::new());
        *embedder.lock() = State::Ready(model);
        embedder
    }

    /// The embedder of the process, keeping the model under the index folder, or nothing when the code index is turned off or there is no cache folder.
    pub fn shared() -> Option<Arc<Embedder>> {
        static SHARED: OnceLock<Option<Arc<Embedder>>> = OnceLock::new();
        SHARED
            .get_or_init(|| {
                if crate::disabled_by_environment() {
                    return None;
                }
                let folder = crate::index_root()?.join("models").join("potion-code-16M");
                Some(Arc::new(Embedder::new(folder)))
            })
            .clone()
    }

    /// The folder the model's files are kept in.
    pub fn folder(&self) -> &Path {
        &self.folder
    }

    /// Locks the state, taking it over from a holder that panicked.
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether every file of the model is on disk.
    fn present(&self) -> bool {
        MODEL_FILES
            .iter()
            .all(|name| self.folder.join(name).is_file())
    }

    /// The model when it is loaded, or can be loaded from disk without downloading anything; this blocks while it loads.
    pub fn loaded(&self) -> Option<Arc<dyn Embed>> {
        let mut state = self.lock();
        match &*state {
            State::Ready(model) => return Some(model.clone()),
            State::Failed(when, _) if when.elapsed() < RETRY_AFTER => return None,
            _ => {}
        }
        if !self.present() {
            return None;
        }
        match StaticEmbedder::load(&self.folder) {
            Ok(model) => {
                let model: Arc<dyn Embed> = Arc::new(model);
                *state = State::Ready(model.clone());
                Some(model)
            }
            Err(error) => {
                *state = State::Failed(Instant::now(), error.to_string());
                None
            }
        }
    }

    /// The model, downloaded first when its files are not on disk.
    ///
    /// A failure is remembered for ten minutes, during which this fails at once with the same reason.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::Download`] when the files cannot be downloaded and [`IndexError::Model`] or [`IndexError::Io`] when they cannot be loaded.
    pub async fn ensure(&self) -> Result<Arc<dyn Embed>, IndexError> {
        if let Some(model) = self.ready()? {
            return Ok(model);
        }
        let _fetching = self.fetching.lock().await;
        if let Some(model) = self.ready()? {
            return Ok(model);
        }
        if !self.present()
            && let Err(error) = fetch(&self.source, &self.folder).await
        {
            *self.lock() = State::Failed(Instant::now(), error.to_string());
            return Err(error);
        }
        let folder = self.folder.clone();
        let loaded = tokio::task::spawn_blocking(move || StaticEmbedder::load(&folder))
            .await
            .map_err(|error| IndexError::Model(error.to_string()))
            .and_then(|loaded| loaded);
        let mut state = self.lock();
        match loaded {
            Ok(model) => {
                let model: Arc<dyn Embed> = Arc::new(model);
                *state = State::Ready(model.clone());
                Ok(model)
            }
            Err(error) => {
                *state = State::Failed(Instant::now(), error.to_string());
                Err(error)
            }
        }
    }

    /// The model when it is loaded, nothing when it is still to be loaded.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::Model`] with the remembered reason when it failed less than ten minutes ago.
    fn ready(&self) -> Result<Option<Arc<dyn Embed>>, IndexError> {
        match &*self.lock() {
            State::Ready(model) => Ok(Some(model.clone())),
            State::Failed(when, reason) if when.elapsed() < RETRY_AFTER => {
                Err(IndexError::Unavailable(reason.clone()))
            }
            _ => Ok(None),
        }
    }
}

/// Downloads the files of the model from `source` into `folder`, each through a partial file renamed into place when it is whole.
///
/// # Errors
///
/// Returns [`IndexError::Download`] when a request fails or is refused and [`IndexError::Io`] when a file cannot be written.
async fn fetch(source: &str, folder: &Path) -> Result<(), IndexError> {
    tokio::fs::create_dir_all(folder)
        .await
        .map_err(|error| IndexError::io(folder, error))?;
    let client = reqwest::Client::builder()
        .user_agent(concat!("crab/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(20))
        .build()
        .map_err(|error| IndexError::Download(error.to_string()))?;
    for name in MODEL_FILES {
        let target = folder.join(name);
        if target.is_file() {
            continue;
        }
        let url = format!("{}/{name}", source.trim_end_matches('/'));
        let mut response = client
            .get(&url)
            .send()
            .await
            .map_err(|error| IndexError::Download(format!("{url}: {error}")))?;
        if !response.status().is_success() {
            return Err(IndexError::Download(format!(
                "{url} answered {}",
                response.status()
            )));
        }
        let partial = folder.join(format!("{name}.part"));
        let mut file = tokio::fs::File::create(&partial)
            .await
            .map_err(|error| IndexError::io(&partial, error))?;
        while let Some(bytes) = response
            .chunk()
            .await
            .map_err(|error| IndexError::Download(format!("{url}: {error}")))?
        {
            file.write_all(&bytes)
                .await
                .map_err(|error| IndexError::io(&partial, error))?;
        }
        file.flush()
            .await
            .map_err(|error| IndexError::io(&partial, error))?;
        drop(file);
        tokio::fs::rename(&partial, &target)
            .await
            .map_err(|error| IndexError::io(&target, error))?;
    }
    Ok(())
}

#[cfg(test)]
/// Tests for loading and downloading the model.
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Serves `body` for every request that asks for a path ending in one of the model's files, and 404 for the rest, on a free local port, returning its address.
    async fn serve(body: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut head = vec![0u8; 4096];
                let read = stream.read(&mut head).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&head[..read]).into_owned();
                let path = request.split_whitespace().nth(1).unwrap_or_default();
                let found = MODEL_FILES.iter().any(|name| path.ends_with(name));
                let (status, text) = if found {
                    ("200 OK", body)
                } else {
                    ("404 No", "")
                };
                let reply = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                );
                let _ = stream.write_all(reply.as_bytes()).await;
            }
        });
        format!("http://{address}/rev")
    }

    /// The folder `name` under the workspace's git-ignored `build/tests/omc-index`, emptied.
    fn scratch(name: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-index")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// The files are downloaded into the folder; files that do not make a model are refused, and the failure is remembered.
    #[tokio::test]
    async fn downloads_then_refuses_a_broken_model() {
        let folder = scratch("embed-fetch");
        let source = serve("not a model").await;
        let embedder = Embedder::from_source(&folder, source);
        assert!(embedder.loaded().is_none());
        let first = embedder.ensure().await.err().unwrap();
        assert!(matches!(first, IndexError::Model(_)), "{first}");
        for name in MODEL_FILES {
            assert_eq!(
                std::fs::read_to_string(folder.join(name)).unwrap(),
                "not a model"
            );
            assert!(!folder.join(format!("{name}.part")).exists());
        }
        let again = embedder.ensure().await.err().unwrap();
        assert_eq!(again.to_string(), first.to_string());
        assert!(embedder.loaded().is_none());
    }

    /// A connection that is refused is a download error.
    #[tokio::test]
    async fn refused_downloads_fail() {
        let folder = scratch("embed-refused");
        let embedder = Embedder::from_source(&folder, "http://127.0.0.1:1/rev");
        let error = embedder.ensure().await.err().unwrap();
        assert!(matches!(error, IndexError::Download(_)), "{error}");
    }

    /// The real model, downloaded into the build folder, embeds related code closer than unrelated text.
    #[tokio::test]
    #[ignore = "downloads the 64 MB model from Hugging Face"]
    async fn the_real_model_embeds_by_meaning() {
        let folder =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../build/tests/omc-index/model");
        let model = Embedder::new(&folder).ensure().await.unwrap();
        let texts: Vec<String> = [
            "parse the configuration file",
            "fn load_config(path: &Path) -> Config { toml::from_str(&read(path)) }",
            "a recipe for banana bread with walnuts",
        ]
        .map(String::from)
        .to_vec();
        let vectors = model.embed(&texts);
        assert_eq!(vectors[0].len(), model.dimensions());
        let near = crate::cosine(&vectors[0], &vectors[1]);
        let far = crate::cosine(&vectors[0], &vectors[2]);
        assert!(near > far, "{near} <= {far}");
    }
}
