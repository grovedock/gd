//! `gd release`: a tag with notes and files.

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use clap::Subcommand;
use gd::remote::{Ch, Remote};
use grovedock_core::Id;
use grovedock_proto::releases_client::ReleasesClient;
use grovedock_proto::{
    AddReleaseAssetRequest, CreateReleaseRequest, DeleteReleaseAssetRequest, ListReleasesRequest, Release,
    ReleaseAsset, ReleaseAssetRef, ReleaseNotesRequest, ReleaseRef, RepositoryRef, UpdateReleaseRequest,
};

#[derive(Subcommand)]
pub enum ReleaseCmd {
    List {
        #[arg(long, default_value_t = 30)]
        limit: u32,
    },
    /// A release (`latest` for the newest that isn't a pre-release).
    View {
        #[arg(default_value = "latest")]
        tag: String,
    },
    /// Notes for a release from what landed since the previous one, and the version that fits.
    Notes {
        /// The tag being released; it needn't exist yet.
        #[arg(default_value = "")]
        tag: String,
        /// Where the release will be: a branch or change id (default: the default branch).
        #[arg(long, default_value = "")]
        at: String,
        /// Notes since this tag (default: the latest published release).
        #[arg(long, default_value = "")]
        since: String,
    },
    /// Release a tag, making it at `--at` if it doesn't exist. Notes are written from what
    /// landed unless `--notes` or `--notes-file` gives them.
    Create {
        tag: String,
        #[arg(long, default_value = "")]
        at: String,
        #[arg(long, default_value = "")]
        title: String,
        #[arg(long)]
        notes: Option<String>,
        #[arg(long, conflicts_with = "notes")]
        notes_file: Option<PathBuf>,
        /// Keep it to people who can write until `gd release publish`.
        #[arg(long)]
        draft: bool,
        #[arg(long)]
        prerelease: bool,
        /// Files to attach (repeatable).
        #[arg(long = "file")]
        files: Vec<PathBuf>,
    },
    /// Publish a draft: it's dated now and announced.
    Publish { tag: String },
    Edit {
        tag: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        notes: Option<String>,
        #[arg(long, conflicts_with = "notes")]
        notes_file: Option<PathBuf>,
        #[arg(long)]
        prerelease: Option<bool>,
    },
    /// Attach files (a file of the same name is replaced).
    Upload { tag: String, files: Vec<PathBuf> },
    /// Download a release's files into a folder (default: here).
    Download {
        tag: String,
        /// Only this file.
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },
    /// A link that downloads one file for ten minutes without signing in (for curl, CI, a
    /// browser).
    Link { tag: String, name: String },
    /// Remove a file from a release.
    Detach { tag: String, name: String },
    /// Delete a release; its tag stays. Published releases: administrators only.
    Delete { tag: String },
}

fn explain(s: tonic::Status) -> anyhow::Error {
    if s.code() == tonic::Code::Unimplemented {
        anyhow!("this server has no releases; they come with `grovedockd server --platform`")
    } else {
        anyhow!("{}", s.message())
    }
}

fn size(n: u64) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.1} GB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.1} KB", n as f64 / 1024.0),
        n => format!("{n} B"),
    }
}

fn status(r: &Release) -> &'static str {
    match (r.draft, r.prerelease, r.latest) {
        (true, ..) => "draft",
        (_, true, _) => "pre-release",
        (_, _, true) => "latest",
        _ => "",
    }
}

fn print(r: &Release) {
    let tag = Id::from_slice(&r.changeset).map(|i| i.short()).unwrap_or_default();
    println!("{} {}{}", r.tag, r.title, if status(r).is_empty() { String::new() } else { format!(" [{}]", status(r)) });
    println!("  {} at {tag}, by {}", if r.draft { "draft" } else { "released" }, r.author);
    for a in &r.assets {
        let sum = if a.sha256.is_empty() { String::new() } else { format!(" sha256 {}", a.sha256) };
        println!("  file {} ({}){sum}", a.name, size(a.size));
    }
    if !r.body.trim().is_empty() {
        println!("\n{}", r.body.trim());
    }
}

fn notes_from(notes: Option<String>, file: Option<PathBuf>) -> Result<Option<String>> {
    Ok(match (notes, file) {
        (Some(n), _) => Some(n),
        (None, Some(f)) => Some(std::fs::read_to_string(&f).with_context(|| format!("reading {}", f.display()))?),
        (None, None) => None,
    })
}

/// Uploads `path` to the repository's store (chunked when large) and attaches it.
async fn attach(
    client: &mut ReleasesClient<Ch>,
    remote: &dyn Remote,
    rf: &ReleaseRef,
    path: &PathBuf,
) -> Result<Release> {
    let name = path.file_name().and_then(|n| n.to_str()).context("a file to attach")?.to_owned();
    let len = std::fs::metadata(path).with_context(|| format!("reading {}", path.display()))?.len();
    let id = if len >= grovedock_core::chunked::CHUNK_THRESHOLD {
        gd::features::commit::upload_large(remote, path).await?.0
    } else {
        let bytes = std::fs::read(path)?;
        let (id, payload) = grovedock_core::object::encode_object(grovedock_core::object::Kind::Blob, &bytes);
        gd::features::commit::upload(remote, &[(id, payload)]).await?;
        id
    };
    let r = client
        .add_release_asset(AddReleaseAssetRequest {
            r#ref: Some(rf.clone()),
            asset: Some(ReleaseAsset { name: name.clone(), blob: id.0.to_vec(), size: len, ..Default::default() }),
        })
        .await
        .map_err(explain)?
        .into_inner();
    eprintln!("attached {name} ({})", size(len));
    Ok(r)
}

pub async fn release(
    mut client: ReleasesClient<Ch>,
    base: &str,
    remote: &dyn Remote,
    repo: RepositoryRef,
    cmd: ReleaseCmd,
) -> Result<()> {
    let rf = |tag: &str| ReleaseRef { repository: Some(repo.clone()), tag: tag.to_owned() };
    match cmd {
        ReleaseCmd::List { limit } => {
            let list = client
                .list_releases(ListReleasesRequest { repository: Some(repo.clone()), limit, ..Default::default() })
                .await
                .map_err(explain)?
                .into_inner();
            if list.releases.is_empty() {
                eprintln!("no releases yet: `gd release notes v0.1.0` drafts the first one's notes");
            }
            for r in list.releases {
                println!("{}\t{}\t{}", r.tag, status(&r), r.title);
            }
        }
        ReleaseCmd::View { tag } => print(&client.get_release(rf(&tag)).await.map_err(explain)?.into_inner()),
        ReleaseCmd::Notes { tag, at, since } => {
            let n = client
                .generate_release_notes(ReleaseNotesRequest {
                    repository: Some(repo.clone()),
                    tag,
                    at,
                    previous: since,
                })
                .await
                .map_err(explain)?
                .into_inner();
            if !n.suggested_version.is_empty() {
                eprintln!(
                    "suggested version: {} ({} changes since {})",
                    n.suggested_version,
                    n.changes,
                    if n.previous_tag.is_empty() { "the start" } else { &n.previous_tag }
                );
            }
            print!("{}", n.body);
        }
        ReleaseCmd::Create { tag, at, title, notes, notes_file, draft, prerelease, files } => {
            let body = match notes_from(notes, notes_file)? {
                Some(b) => b,
                None => {
                    client
                        .generate_release_notes(ReleaseNotesRequest {
                            repository: Some(repo.clone()),
                            tag: tag.clone(),
                            at: at.clone(),
                            ..Default::default()
                        })
                        .await
                        .map_err(explain)?
                        .into_inner()
                        .body
                }
            };
            // Files go on a draft first, so a published release never shows half its files.
            let publish_after = !draft && !files.is_empty();
            let mut r = client
                .create_release(CreateReleaseRequest {
                    repository: Some(repo.clone()),
                    tag: tag.clone(),
                    at,
                    title,
                    body,
                    draft: draft || publish_after,
                    prerelease,
                })
                .await
                .map_err(explain)?
                .into_inner();
            for f in &files {
                r = attach(&mut client, remote, &rf(&tag), f).await?;
            }
            if publish_after {
                r = client
                    .update_release(UpdateReleaseRequest {
                        r#ref: Some(rf(&tag)),
                        fields: vec!["draft".into()],
                        ..Default::default()
                    })
                    .await
                    .map_err(explain)?
                    .into_inner();
            }
            print(&r);
        }
        ReleaseCmd::Publish { tag } => {
            let r = client
                .update_release(UpdateReleaseRequest {
                    r#ref: Some(rf(&tag)),
                    fields: vec!["draft".into()],
                    ..Default::default()
                })
                .await
                .map_err(explain)?
                .into_inner();
            print(&r);
        }
        ReleaseCmd::Edit { tag, title, notes, notes_file, prerelease } => {
            let mut req = UpdateReleaseRequest { r#ref: Some(rf(&tag)), ..Default::default() };
            if let Some(t) = title {
                req.fields.push("title".into());
                req.title = t;
            }
            if let Some(b) = notes_from(notes, notes_file)? {
                req.fields.push("body".into());
                req.body = b;
            }
            if let Some(p) = prerelease {
                req.fields.push("prerelease".into());
                req.prerelease = p;
            }
            if req.fields.is_empty() {
                anyhow::bail!("nothing to change: --title, --notes, --notes-file or --prerelease");
            }
            print(&client.update_release(req).await.map_err(explain)?.into_inner());
        }
        ReleaseCmd::Upload { tag, files } => {
            if files.is_empty() {
                anyhow::bail!("name the files to attach");
            }
            let mut last = None;
            for f in &files {
                last = Some(attach(&mut client, remote, &rf(&tag), f).await?);
            }
            if let Some(r) = last {
                print(&r);
            }
        }
        ReleaseCmd::Download { tag, name, dir } => {
            let r = client.get_release(rf(&tag)).await.map_err(explain)?.into_inner();
            let wanted: Vec<&ReleaseAsset> =
                r.assets.iter().filter(|a| name.as_ref().is_none_or(|n| *n == a.name)).collect();
            if wanted.is_empty() {
                anyhow::bail!(
                    "{} has no {}",
                    r.tag,
                    name.map(|n| format!("file {n}")).unwrap_or_else(|| "files".into())
                );
            }
            std::fs::create_dir_all(&dir)?;
            for a in wanted {
                let id = Id::from_slice(&a.blob)?;
                let payload =
                    remote.objects().get(&id).await?.with_context(|| format!("{} is missing on the server", a.name))?;
                let out = dir.join(&a.name);
                let mut file = std::io::BufWriter::new(std::fs::File::create(&out)?);
                grovedock_core::chunked::write_content(remote.objects().as_ref(), &id, &payload, &mut file).await?;
                println!("{} ({})", out.display(), size(a.size));
            }
        }
        ReleaseCmd::Link { tag, name } => {
            let l = client
                .release_asset_url(ReleaseAssetRef { r#ref: Some(rf(&tag)), name })
                .await
                .map_err(explain)?
                .into_inner();
            println!("{}{}", base.trim_end_matches('/'), l.url);
        }
        ReleaseCmd::Detach { tag, name } => {
            let r = client
                .delete_release_asset(DeleteReleaseAssetRequest { r#ref: Some(rf(&tag)), name })
                .await
                .map_err(explain)?
                .into_inner();
            print(&r);
        }
        ReleaseCmd::Delete { tag } => {
            client.delete_release(rf(&tag)).await.map_err(explain)?;
            println!("deleted release {tag}; the tag stays");
        }
    }
    Ok(())
}
