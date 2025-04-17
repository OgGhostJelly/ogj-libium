pub mod check;
pub mod mod_downloadable;

use crate::{
    config::{
        modpack::modrinth,
        structs::{
            ModLoader, ProfileImport, ProfileImportSource, ReleaseChannel, SourceId, SourceKind,
            SourceKindWithModpack,
        },
    },
    iter_ext::IterExt as _,
    version_ext::VersionExt,
    TMP_DIR,
};
use ferinth::structures::{
    project::ProjectType,
    version::{
        DependencyType as MRDependencyType, Hash as MRHash, Version as MRVersion, VersionType,
    },
};
use furse::structures::file_structs::{
    File as CFFile, FileHash as CFHash, FileRelationType as CFFileRelationType, FileReleaseType,
    HashAlgo as CFHashAlgo,
};
use md5::Digest;
use octocrab::models::repos::{Asset as GHAsset, Release as GHRelease};
use reqwest::{Client, Url};
use std::{
    ffi::OsStr,
    fs::{self, create_dir_all, rename, File, OpenOptions},
    io::{self, BufWriter, SeekFrom, Write},
    path::{Path, PathBuf},
    str::FromStr,
};

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub enum Error {
    ReqwestError(#[from] reqwest::Error),
    IOError(#[from] std::io::Error),
    #[error("expected file hash {0} but got {1}")]
    UnexpectedFileHash(String, String),
}
type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone)]
pub struct Metadata {
    /// The title of the GitHub Release, Modrinth Version, or CurseForge File
    pub title: String,
    /// The body of the GitHub Release, or the changelog of the Modrinth Version
    pub description: String,
    pub filename: String,

    pub channel: ReleaseChannel,

    pub game_versions: Vec<String>,
    pub loaders: Vec<ModLoader>,
}

/// Downloadable data from a source on the internet.
#[derive(Debug, Clone)]
pub struct DownloadData {
    pub src: DownloadSource,
    /// The path of the downloaded file relative to the output directory
    ///
    /// The filename by default, but can be configured with subdirectories for modpacks.
    pub output: PathBuf,
    /// The length of the file in bytes
    pub length: u64,
    /// The dependencies this file has
    pub dependencies: Vec<SourceId>,
    /// Other mods this file is incompatible with
    pub conflicts: Vec<SourceId>,
    /// The kind of source file, `None` if the kind is unknown.
    pub kind: Option<SourceKindWithModpack>,
    /// The expected hash of the file.
    /// The hash is provided by the source (e.g Github)
    /// and is recalculated and compared when downloading.
    pub hash: Option<Hash>,
    /// User-provided hashes in sha512 (base16) format.
    /// The hash is calculated and compared when downloading.
    /// If any of the hashes are not equal, an error will be raised.
    pub user_hash: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum Hash {
    Curseforge(Vec<CFHash>),
    Modrinth(MRHash),
}

impl Hash {
    /// Compare the hash to a reader object.
    /// If the reader is a file, there is no guarantee where the file cursor will end up.
    fn compare<R>(&self, reader: &mut R) -> Result<()>
    where
        R: io::Read + io::Seek,
    {
        match self {
            Hash::Curseforge(hashes) => {
                for hash in hashes {
                    match hash.algo {
                        CFHashAlgo::Sha1 => {
                            Self::compare_hash::<sha1::Sha1, _>(&hash.value, reader)?
                        }
                        CFHashAlgo::Md5 => Self::compare_hash::<md5::Md5, _>(&hash.value, reader)?,
                    }
                    reader.seek(SeekFrom::Start(0))?;
                }
            }
            Hash::Modrinth(hash) => {
                Self::compare_hash::<sha1::Sha1, _>(&hash.sha1, reader)?;
                reader.seek(SeekFrom::Start(0))?;
                Self::compare_hash::<sha2::Sha512, _>(&hash.sha512, reader)?;
            }
        }

        Ok(())
    }

    fn compare_hash<D, R>(expected: &str, reader: &mut R) -> Result<()>
    where
        D: Digest + io::Write,
        R: ?Sized + io::Read,
    {
        let mut hasher = D::new();
        io::copy(reader, &mut hasher)?;
        let hash = hasher.finalize();
        let got = base16ct::lower::encode_string(&hash);
        if expected != got {
            return Err(Error::UnexpectedFileHash(expected.to_string(), got));
        }
        Ok(())
    }
}

/// The source of some downloadable data.
#[derive(Debug, Clone)]
pub enum DownloadSource {
    Url(Url),
    Contents(String),
    Path(PathBuf),
}

#[derive(Debug, thiserror::Error)]
#[error("The developer of this project has denied third party applications from downloading it")]
/// Contains the mod ID and file ID
pub struct DistributionDeniedError(pub i32, pub i32);

pub fn try_from_cf_file(
    kind: SourceKind,
    file: CFFile,
    class_id: Option<usize>,
) -> std::result::Result<(Metadata, DownloadData), DistributionDeniedError> {
    Ok((
        Metadata {
            title: file.display_name,
            description: String::new(), // Changelog requires a separate request
            filename: file.file_name.clone(),
            channel: match file.release_type {
                FileReleaseType::Release => ReleaseChannel::Release,
                FileReleaseType::Beta => ReleaseChannel::Beta,
                FileReleaseType::Alpha => ReleaseChannel::Alpha,
            },
            loaders: file
                .game_versions
                .iter()
                .filter_map(|s| ModLoader::from_str(s).ok())
                .collect_vec(),
            game_versions: file.game_versions,
        },
        DownloadData {
            src: DownloadSource::Url(
                file.download_url
                    .ok_or(DistributionDeniedError(file.mod_id, file.id))?,
            ),
            output: kind.directory().join(file.file_name.as_str()),
            length: file.file_length as u64,
            dependencies: file
                .dependencies
                .iter()
                .filter_map(|d| {
                    if d.relation_type == CFFileRelationType::RequiredDependency {
                        Some(SourceId::Curseforge(d.mod_id))
                    } else {
                        None
                    }
                })
                .collect_vec(),
            conflicts: file
                .dependencies
                .iter()
                .filter_map(|d| {
                    if d.relation_type == CFFileRelationType::Incompatible {
                        Some(SourceId::Curseforge(d.mod_id))
                    } else {
                        None
                    }
                })
                .collect_vec(),
            kind: class_id.and_then(SourceKindWithModpack::from_cf_class_id),
            hash: Some(Hash::Curseforge(file.hashes)),
            user_hash: vec![],
        },
    ))
}

pub fn from_mr_version(
    kind: SourceKind,
    version: MRVersion,
    project_type: Option<ProjectType>,
) -> (Metadata, DownloadData) {
    (
        Metadata {
            title: version.name.clone(),
            description: version.changelog.as_ref().cloned().unwrap_or_default(),
            filename: version.get_version_file().filename.clone(),
            channel: match version.version_type {
                VersionType::Release => ReleaseChannel::Release,
                VersionType::Beta => ReleaseChannel::Beta,
                VersionType::Alpha => ReleaseChannel::Alpha,
            },
            loaders: version
                .loaders
                .iter()
                .filter_map(|s| ModLoader::from_str(s).ok())
                .collect_vec(),

            game_versions: version.game_versions.clone(),
        },
        DownloadData {
            src: DownloadSource::Url(version.get_version_file().url.clone()),
            output: kind
                .directory()
                .join(version.get_version_file().filename.as_str()),
            length: version.get_version_file().size as u64,
            dependencies: version
                .dependencies
                .clone()
                .into_iter()
                .filter_map(|d| {
                    if d.dependency_type == MRDependencyType::Required {
                        match (d.project_id, d.version_id) {
                            (Some(proj_id), Some(ver_id)) => {
                                Some(SourceId::PinnedModrinth(proj_id, ver_id))
                            }
                            (Some(proj_id), None) => Some(SourceId::Modrinth(proj_id)),
                            _ => None,
                        }
                    } else {
                        None
                    }
                })
                .collect_vec(),
            hash: Some(Hash::Modrinth(version.get_version_file().hashes.clone())),
            conflicts: version
                .dependencies
                .into_iter()
                .filter_map(|d| {
                    if d.dependency_type == MRDependencyType::Incompatible {
                        match (d.project_id, d.version_id) {
                            (Some(proj_id), Some(ver_id)) => {
                                Some(SourceId::PinnedModrinth(proj_id, ver_id))
                            }
                            (Some(proj_id), None) => Some(SourceId::Modrinth(proj_id)),
                            _ => None,
                        }
                    } else {
                        None
                    }
                })
                .collect_vec(),
            kind: project_type.and_then(SourceKindWithModpack::from_mr_project_type),
            user_hash: vec![],
        },
    )
}

pub fn from_gh_releases(
    kind: SourceKind,
    releases: impl IntoIterator<Item = GHRelease>,
) -> Vec<(Metadata, DownloadData)> {
    releases
        .into_iter()
        .flat_map(|release| {
            release.assets.into_iter().map(move |asset| {
                (
                    Metadata {
                        title: release.name.clone().unwrap_or_default(),
                        description: release.body.clone().unwrap_or_default(),
                        channel: if release.prerelease {
                            ReleaseChannel::Beta
                        } else {
                            ReleaseChannel::Release
                        },
                        game_versions: asset
                            .name
                            .trim_end_matches(".jar")
                            .trim_end_matches(".zip")
                            .split(['-', '_', '+'])
                            .map(|s| s.trim_start_matches("mc"))
                            .filter(|s| semver::Version::from_str(s).is_ok())
                            .map(ToOwned::to_owned)
                            .collect_vec(),
                        loaders: asset
                            .name
                            .trim_end_matches(".jar")
                            .trim_end_matches(".zip")
                            .split(['-', '_', '+'])
                            .filter_map(|s| ModLoader::from_str(s).ok())
                            .collect_vec(),
                        filename: asset.name.clone(),
                    },
                    from_gh_asset(kind, asset),
                )
            })
        })
        .collect_vec()
}

pub fn from_gh_asset(kind: SourceKind, asset: GHAsset) -> DownloadData {
    DownloadData {
        src: DownloadSource::Url(asset.browser_download_url),
        output: kind.directory().join(asset.name),
        length: asset.size as u64,
        dependencies: Vec::new(),
        conflicts: Vec::new(),
        kind: None,
        hash: None,
        user_hash: vec![],
    }
}

pub fn from_file(
    kind: SourceKind,
    src_path: &Path,
    path: &Path,
) -> Result<(Metadata, DownloadData)> {
    let path = src_path.join(path);

    let length = File::open(&path)?.metadata()?.len();
    let filename = path.file_name().unwrap_or(OsStr::new("")).to_os_string();
    let output = kind.directory().join(&filename);
    let inferred_kind = SourceKindWithModpack::infer(&path)?;

    Ok((
        Metadata {
            title: {
                let filename = filename.to_string_lossy();
                let (title, _) = filename.split_once('.').unwrap_or((&filename, ""));
                title.to_string()
            },
            description: format!("File at path {}", path.display()),
            filename: filename
                .to_str()
                .map(ToString::to_string)
                .expect("Filename has invalid unicode"),
            channel: ReleaseChannel::Release,
            game_versions: vec![],
            loaders: vec![],
        },
        DownloadData {
            src: DownloadSource::Path(path),
            output,
            length,
            dependencies: vec![],
            conflicts: vec![],
            kind: inferred_kind,
            hash: None,
            user_hash: vec![],
        },
    ))
}

pub async fn from_url(kind: SourceKind, url: &Url) -> Result<(Metadata, DownloadData)> {
    let path = url.path();
    let (_, filename) = path.split_once('/').unwrap_or(("", path));
    let (title, _) = filename.split_once('.').unwrap_or((filename, ""));
    let output = kind.directory().join(filename);

    let length = reqwest::get(url.clone())
        .await?
        .content_length()
        .unwrap_or(0);

    Ok((
        Metadata {
            title: title.to_string(),
            description: format!("File at url {url}"),
            filename: filename.to_string(),
            channel: ReleaseChannel::Release,
            game_versions: vec![],
            loaders: vec![],
        },
        DownloadData {
            src: DownloadSource::Url(url.clone()),
            output,
            length,
            dependencies: vec![],
            conflicts: vec![],
            kind: None,
            hash: None,
            user_hash: vec![],
        },
    ))
}

pub fn from_modpack_file(file: modrinth::ModpackFile) -> DownloadData {
    DownloadData {
        src: DownloadSource::Url(
            file.downloads
                .first()
                .expect("Download URLs not provided")
                .clone(),
        ),
        output: file.path,
        length: file.file_size as u64,
        dependencies: Vec::new(),
        conflicts: Vec::new(),
        kind: None,
        hash: Some(Hash::Modrinth(file.hashes)),
        user_hash: vec![],
    }
}

impl DownloadData {
    /// Consumes `self` and downloads the file to the `output_dir`
    ///
    /// The `update` closure is called with the chunk length whenever a chunk is downloaded and written.
    ///
    /// Returns the total size of the file and the filename.
    pub async fn download(
        self,
        client: Client,
        output_dir: impl AsRef<Path>,
        update: impl Fn(usize) + Send,
    ) -> Result<(usize, String)> {
        let (filename, src, size) = (self.filename(), self.src, self.length);
        let out_file_path = output_dir.as_ref().join(&self.output);
        let temp_file_path = out_file_path.with_extension("part");
        if let Some(up_dir) = out_file_path.parent() {
            create_dir_all(up_dir)?;
        }

        let mut temp_file = BufWriter::with_capacity(
            size as usize,
            OpenOptions::new()
                .append(true)
                .create(true)
                .open(&temp_file_path)?,
        );

        match src {
            DownloadSource::Url(url) => {
                let mut response = client.get(url).send().await?;
                while let Some(chunk) = response.chunk().await? {
                    temp_file.write_all(&chunk)?;
                    update(chunk.len());
                }
            }
            DownloadSource::Contents(data) => {
                let data = data.as_bytes();
                temp_file.write_all(data)?;
                update(data.len());
            }
            DownloadSource::Path(path) => {
                let bytes = fs::copy(&path, &temp_file_path)?;
                update(bytes as usize);
            }
        };

        temp_file.flush()?;

        if let Some(hash) = self.hash {
            hash.compare(&mut File::open(&temp_file_path)?)?;
        }

        if !self.user_hash.is_empty() {
            let hash = calculate_sha512(&temp_file_path)?;
            for expected in self.user_hash {
                if !hash.starts_with(&expected.to_ascii_lowercase()) {
                    return Err(Error::UnexpectedFileHash(expected, hash));
                }
            }
        }

        rename(temp_file_path, out_file_path)?;
        Ok((size as usize, filename))
    }

    pub fn filename(&self) -> String {
        self.output
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
    }
}

pub fn calculate_sha512(path: &Path) -> std::result::Result<String, io::Error> {
    let mut hasher = sha2::Sha512::new();
    io::copy(&mut File::open(path)?, &mut hasher)?;
    Ok(base16ct::lower::encode_string(&hasher.finalize()))
}

impl ProfileImport {
    pub async fn download(&self) -> Result<PathBuf> {
        match self {
            ProfileImport::Short(src) => src.download().await,
            ProfileImport::Long { src, hash } => {
                let path = src.download().await?;
                let file_hash = calculate_sha512(&path)?;
                if !file_hash.starts_with(&hash.to_ascii_lowercase()) {
                    return Err(Error::UnexpectedFileHash(hash.clone(), file_hash));
                }
                Ok(path)
            }
        }
    }
}

impl ProfileImportSource {
    pub async fn download(&self) -> Result<PathBuf> {
        match self {
            ProfileImportSource::Path(path) => Ok(path.clone()),
            ProfileImportSource::Url(url) => {
                let path = url.path();
                let (_, filename) = path.split_once('/').unwrap_or(("", path));
                let temp_file_path = TMP_DIR.join(filename);

                let mut temp_file = File::create(&temp_file_path)?;
                let mut response = reqwest::get(url.clone()).await?;
                while let Some(chunk) = response.chunk().await? {
                    temp_file.write_all(&chunk)?;
                }

                Ok(temp_file_path)
            }
        }
    }
}
