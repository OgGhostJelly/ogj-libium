use super::{
    check, from_gh_releases, from_mr_version, try_from_cf_file, DistributionDeniedError,
    DownloadData,
};
use crate::{
    config::structs::{Filters, Source, SourceId},
    iter_ext::IterExt as _,
    CURSEFORGE_API, GITHUB_API, MODRINTH_API,
};
use std::cmp::Reverse;

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub enum Error {
    DistributionDenied(#[from] DistributionDeniedError),
    CheckError(#[from] super::check::Error),
    #[error("The pin provided is an invalid identifier")]
    InvalidPinID(#[from] std::num::ParseIntError),
    #[error("Modrinth: {0}")]
    ModrinthError(#[from] ferinth::Error),
    #[error("CurseForge: {0}")]
    CurseForgeError(#[from] furse::Error),
    #[error("GitHub: {0:#?}")]
    GitHubError(#[from] octocrab::Error),
    #[error("No compatible mod sources found")]
    NoCompatibleSources,
}
type Result<T> = std::result::Result<T, Error>;

impl Source {
    pub async fn fetch_download_file(&self, filters: Vec<&Filters>) -> Result<DownloadData> {
        let mut sources = vec![];
        let _ = self.each_sources(filters, |filters, id| {
            sources.push((filters, id));
        });

        let mut download_files = vec![];
        for (filters, id) in sources {
            let fut = id.fetch_download_file(filters);
            download_files.push(fut);
        }

        for file in download_files {
            match file.await {
                Ok(data) => return Ok(data),
                Err(Error::CheckError(check::Error::IntersectFailure)) => {}
                Err(e) => return Err(e),
            }
        }

        Err(Error::NoCompatibleSources)
    }
}

impl SourceId {
    pub async fn fetch_download_file(&self, filters: Vec<&Filters>) -> Result<DownloadData> {
        let download_files = match self {
            SourceId::Curseforge(id) => {
                let mut files = CURSEFORGE_API.get_mod_files(*id).await?;
                files.sort_unstable_by_key(|f| Reverse(f.file_date));
                files
                    .into_iter()
                    .map(|f| try_from_cf_file(f).map_err(Into::into))
                    .collect::<Result<Vec<_>>>()?
            }
            SourceId::Modrinth(id) => MODRINTH_API
                .list_versions(id)
                .await?
                .into_iter()
                .map(from_mr_version)
                .collect_vec(),
            SourceId::Github(owner, repo) => GITHUB_API
                .repos(owner, repo)
                .releases()
                .list()
                .send()
                .await
                .map(|r| from_gh_releases(r.items))?,
            _ => todo!(),
            /* TODO: Add pinned sources
            ModIdentifier::PinnedCurseForgeProject(mod_id, pin) => {
                Ok(try_from_cf_file(CURSEFORGE_API.get_mod_file(*mod_id, *pin).await?)?.1)
            }
            ModIdentifier::PinnedModrinthProject(_, pin) => {
                Ok(from_mr_version(MODRINTH_API.get_version(pin).await?).1)
            }
            ModIdentifier::PinnedGitHubRepository((owner, repo), pin) => Ok(from_gh_asset(
                GITHUB_API
                    .repos(owner, repo)
                    .release_assets()
                    .get(*pin as u64)
                    .await?,
            )), */
        };

        let index =
            super::check::select_latest(download_files.iter().map(|(m, _)| m), filters).await?;
        Ok(download_files.into_iter().nth(index).unwrap().1)
    }
}
