//! Remote port: everything the client needs from the server, behind one trait so the
//! transport (gRPC today; HTTP/JSON, in-process, a mock) is replaceable.

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use grovedock_adapters::grpc::{GrpcObjects, status_err};
use grovedock_core::acl::ReadScope;
use grovedock_core::object::{Changeset, FileChange, Mode};
use grovedock_core::path::components as validate_path;
use grovedock_core::ports::ObjectStore;
use grovedock_core::{Id, path::validate_name};
use grovedock_proto::access_client::AccessClient;
use grovedock_proto::browse_client::BrowseClient;
use grovedock_proto::changesets_client::ChangesetsClient;
use grovedock_proto::diff_client::DiffClient;
use grovedock_proto::history_client::HistoryClient;
use grovedock_proto::land_client::LandClient;
use grovedock_proto::locks_client::LocksClient;
use grovedock_proto::merge_base_client::MergeBaseClient;
use grovedock_proto::refs_client::RefsClient;
use grovedock_proto::{
    BearerAuth, BlameRequest, BrowseRequest, CasRequest, CheckoutRequest, CreateChangesetRequest, DiffRequest, Empty,
    FindRequest, LandRequest, ListLocksRequest, LockRequest, LogRequest, MAX_MESSAGE_BYTES, MergeBaseRequest, RefName,
    ResolveRequest, UnlockRequest,
};
use tonic::service::interceptor::InterceptedService;
use tonic::transport::{Channel, Endpoint};

/// A changeset as seen by the client: verified content plus server-derived data.
#[derive(Debug, Clone)]
pub struct RemoteChangeset {
    pub id: Id,
    /// Changes outside the caller's read scope were removed by the server.
    pub redacted: bool,
    pub root_tree: Id,
    pub generation: u64,
    pub changeset: Changeset,
    /// Who uploaded it, as the server recorded it (changes made with gd, since provenance is kept).
    pub provenance: Option<grovedock_proto::Provenance>,
}

/// A piece of a file from a path-scoped checkout; `last` ends the file.
#[derive(Debug, Clone)]
pub struct Piece {
    pub path: String,
    pub mode: Mode,
    pub id: Id,
    pub data: Vec<u8>,
    pub last: bool,
}

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub mode: Mode,
    /// Absent for directories the caller may traverse but not read.
    pub id: Option<Id>,
}

#[async_trait]
pub trait Remote: Send + Sync {
    /// Object access; may point at a node daemon or regional cache instead of the server.
    fn objects(&self) -> Arc<dyn ObjectStore>;
    async fn create_changeset(&self, cs: Changeset) -> Result<RemoteChangeset>;
    /// The same, with what an agent says of itself (model, session, issue) recorded by the server
    /// beside the change, not in its message.
    async fn create_changeset_declared(
        &self,
        cs: Changeset,
        _model: &str,
        _session: &str,
        _task: u64,
    ) -> Result<RemoteChangeset> {
        self.create_changeset(cs).await
    }
    async fn changeset(&self, rev: &str) -> Result<RemoteChangeset>;
    async fn get_ref(&self, name: &str) -> Result<Option<Id>>;
    async fn compare_and_swap(&self, name: &str, expected: Option<Id>, new: Id) -> Result<bool>;
    async fn list_refs(&self) -> Result<Vec<(String, Id)>>;
    async fn land(&self, ref_name: &str, cs: Id) -> Result<(Id, bool)>;
    /// Moves `ref_name` to `cs` as it is if its head is an ancestor (as git pushes), with every
    /// changeset it brings checked as a land would be; refused otherwise.
    async fn fast_forward(&self, ref_name: &str, cs: Id) -> Result<Id>;
    async fn log(&self, rev: &str, path: &str, limit: u32) -> Result<Vec<RemoteChangeset>>;
    async fn diff(&self, from: &str, to: &str) -> Result<Vec<FileChange>>;
    async fn merge_base(&self, a: &str, b: &str) -> Result<Option<Id>>;
    async fn list_dir(&self, rev: &str, path: &str) -> Result<Vec<DirEntry>>;
    async fn read_file(&self, rev: &str, path: &str) -> Result<(Mode, Vec<u8>)>;
    /// The caller's read scope.
    async fn whoami(&self) -> Result<ReadScope>;
    /// Stream readable files under `paths` (everything readable when empty) at `rev`.
    async fn checkout(
        &self,
        rev: &str,
        paths: &[String],
        sink: &mut (dyn FnMut(Piece) -> Result<()> + Send),
    ) -> Result<()>;
    async fn find(&self, rev: &str, pattern: &str, limit: u32) -> Result<Vec<String>>;
    /// Lock a file for exclusive editing; returns (path, owner, locked at ms).
    async fn lock(&self, path: &str) -> Result<(String, String, i64)>;
    async fn unlock(&self, path: &str, force: bool) -> Result<()>;
    /// Locks under `prefix` ("" for all).
    async fn locks(&self, prefix: &str) -> Result<Vec<(String, String, i64)>>;
    /// (changeset that last changed the line, line text) for each line.
    /// The git-ai note kept with a change that came from git, if any.
    async fn git_ai_note(&self, _change: Id) -> Result<Option<String>> {
        Ok(None)
    }

    /// Each line with the changeset that last changed it, its text, and who wrote it ("" when
    /// only the changeset's author is known).
    async fn blame(&self, rev: &str, path: &str) -> Result<Vec<(Id, String, String)>>;
}

/// An authenticated channel to a server.
pub type Ch = InterceptedService<Channel, BearerAuth>;

#[derive(Clone)]
pub struct GrpcRemote {
    channel: Channel,
    auth: BearerAuth,
    objects: Arc<dyn ObjectStore>,
}

fn verify(info: grovedock_proto::ChangesetInfo) -> Result<RemoteChangeset> {
    let id = Id::from_slice(&info.id)?;
    // A redacted changeset (changes outside our read scope removed) cannot hash to its id;
    // everything else is verified.
    let changeset = if info.redacted {
        <Changeset as prost::Message>::decode(&info.encoded[..])?
    } else {
        Changeset::decode_verified(&id, &info.encoded)?
    };
    Ok(RemoteChangeset {
        changeset,
        redacted: info.redacted,
        root_tree: Id::from_slice(&info.root_tree)?,
        generation: info.generation,
        provenance: info.provenance,
        id,
    })
}

fn wire_change(c: FileChange) -> grovedock_proto::FileChange {
    grovedock_proto::FileChange { path: c.path, blob: c.blob, mode: c.mode }
}

impl GrpcRemote {
    /// `server` hosts all services; `objects_url` (daemon or cache) serves object reads,
    /// defaulting to the server.
    /// `server` may end in `/OWNER/NAME` to name a repository on a server that hosts many.
    /// Without a `token`, the one `gd login` saved for the server is used, if any.
    pub fn connect(server: &str, objects_url: Option<&str>, token: Option<&str>) -> Result<Self> {
        let token = crate::credentials::resolve(token, server);
        let token = token.as_deref();
        let (base, repo) = grovedock_proto::split_repo_url(server);
        let channel = Endpoint::from_shared(base)
            .with_context(|| format!("bad server URL `{server}`"))?
            .tcp_nodelay(true)
            .http2_adaptive_window(true)
            .connect_lazy();
        let auth =
            BearerAuth::new(token).and_then(|a| a.with_repo(repo.as_deref())).map_err(|s| anyhow::anyhow!("{s}"))?;
        let objects: Arc<dyn ObjectStore> = match objects_url {
            Some(url) => Arc::new(GrpcObjects::connect(url, token)?),
            None => Arc::new(GrpcObjects::with_auth(channel.clone(), auth.clone())),
        };
        Ok(Self { channel, auth, objects })
    }

    fn ch(&self) -> Ch {
        InterceptedService::new(self.channel.clone(), self.auth.clone())
    }

    /// The hosting API: repositories and organisations (servers started with `--platform`).
    pub fn platform(&self) -> grovedock_proto::platform_client::PlatformClient<Ch> {
        grovedock_proto::platform_client::PlatformClient::new(self.ch())
    }

    pub fn reviews(&self) -> grovedock_proto::reviews_client::ReviewsClient<Ch> {
        grovedock_proto::reviews_client::ReviewsClient::new(self.ch())
    }

    pub fn issues(&self) -> grovedock_proto::issues_client::IssuesClient<Ch> {
        grovedock_proto::issues_client::IssuesClient::new(self.ch())
    }

    pub fn status(&self) -> grovedock_proto::status_client::StatusClient<Ch> {
        grovedock_proto::status_client::StatusClient::new(self.ch())
    }

    /// Keeps a git-ai note (which lines an agent wrote) with a change that came from git.
    pub async fn attach_git_ai_note(&self, change: Id, note: &str) -> Result<()> {
        HistoryClient::new(self.ch())
            .attach_git_ai_note(grovedock_proto::AttachGitAiNoteRequest {
                changeset: change.0.to_vec(),
                note: note.to_owned(),
            })
            .await
            .map_err(st)?;
        Ok(())
    }

    pub fn packets(&self) -> grovedock_proto::packets_client::PacketsClient<Ch> {
        grovedock_proto::packets_client::PacketsClient::new(self.ch())
    }

    pub fn landing(&self) -> grovedock_proto::landing_client::LandingClient<Ch> {
        grovedock_proto::landing_client::LandingClient::new(self.ch())
    }

    pub fn runs(&self) -> grovedock_proto::runs_client::RunsClient<Ch> {
        grovedock_proto::runs_client::RunsClient::new(self.ch())
    }

    pub fn releases(&self) -> grovedock_proto::releases_client::ReleasesClient<Ch> {
        grovedock_proto::releases_client::ReleasesClient::new(self.ch())
    }

    /// Stars, watching, people's pages and Explore.
    pub fn social(&self) -> grovedock_proto::social_client::SocialClient<Ch> {
        grovedock_proto::social_client::SocialClient::new(self.ch())
    }

    /// Bringing repositories from other hosts (the importer).
    pub fn imports(&self) -> grovedock_proto::imports_client::ImportsClient<Ch> {
        grovedock_proto::imports_client::ImportsClient::new(self.ch())
    }

    /// How an organisation's work flows (its administrators).
    pub fn insights(&self) -> grovedock_proto::insights_client::InsightsClient<Ch> {
        grovedock_proto::insights_client::InsightsClient::new(self.ch())
    }

    /// Code search over the repository this remote names.
    pub fn code_search(&self) -> grovedock_proto::code_search_client::CodeSearchClient<Ch> {
        grovedock_proto::code_search_client::CodeSearchClient::new(self.ch())
    }
}

fn st(s: tonic::Status) -> anyhow::Error {
    status_err(s).into()
}

#[async_trait]
impl Remote for GrpcRemote {
    fn objects(&self) -> Arc<dyn ObjectStore> {
        self.objects.clone()
    }

    async fn create_changeset(&self, cs: Changeset) -> Result<RemoteChangeset> {
        // An agent driving the CLI says what it is with GD_MODEL, GD_SESSION and GD_TASK.
        let env = |k: &str| std::env::var(k).unwrap_or_default();
        let task = env("GD_TASK").trim_start_matches('#').parse().unwrap_or(0);
        self.create_changeset_declared(cs, &env("GD_MODEL"), &env("GD_SESSION"), task).await
    }

    async fn create_changeset_declared(
        &self,
        cs: Changeset,
        model: &str,
        session: &str,
        task: u64,
    ) -> Result<RemoteChangeset> {
        let req = CreateChangesetRequest {
            model: model.to_owned(),
            session: session.to_owned(),
            task,
            parents: cs.parents,
            changes: cs.changes.into_iter().map(wire_change).collect(),
            author: cs.author,
            message: cs.message,
            timestamp_ms: cs.timestamp_ms,
            git: cs.git.map(|g| prost::Message::encode_to_vec(&g)).unwrap_or_default(),
        };
        let mut c = ChangesetsClient::new(self.ch()).max_encoding_message_size(MAX_MESSAGE_BYTES);
        verify(c.create(req).await.map_err(st)?.into_inner())
    }

    async fn changeset(&self, rev: &str) -> Result<RemoteChangeset> {
        let r = ChangesetsClient::new(self.ch()).get(ResolveRequest { rev: rev.to_owned() }).await.map_err(st)?;
        verify(r.into_inner())
    }

    async fn get_ref(&self, name: &str) -> Result<Option<Id>> {
        let r = RefsClient::new(self.ch()).get(RefName { name: name.to_owned() }).await.map_err(st)?.into_inner();
        if r.exists { Ok(Some(Id::from_slice(&r.id)?)) } else { Ok(None) }
    }

    async fn compare_and_swap(&self, name: &str, expected: Option<Id>, new: Id) -> Result<bool> {
        let req = CasRequest {
            name: name.to_owned(),
            expect_exists: expected.is_some(),
            expected: expected.map(|i| i.0.to_vec()).unwrap_or_default(),
            new: new.0.to_vec(),
        };
        Ok(RefsClient::new(self.ch()).compare_and_swap(req).await.map_err(st)?.into_inner().swapped)
    }

    async fn list_refs(&self) -> Result<Vec<(String, Id)>> {
        let r = RefsClient::new(self.ch()).list(Empty {}).await.map_err(st)?.into_inner();
        r.refs.into_iter().map(|e| Ok((e.name, Id::from_slice(&e.id)?))).collect()
    }

    async fn fast_forward(&self, ref_name: &str, cs: Id) -> Result<Id> {
        let req = LandRequest { r#ref: ref_name.to_owned(), changeset: cs.0.to_vec(), fast_forward: true };
        let r = LandClient::new(self.ch()).land(req).await.map_err(st)?.into_inner();
        Ok(Id::from_slice(&r.landed)?)
    }

    async fn land(&self, ref_name: &str, cs: Id) -> Result<(Id, bool)> {
        let req = LandRequest { r#ref: ref_name.to_owned(), changeset: cs.0.to_vec(), fast_forward: false };
        let r = LandClient::new(self.ch()).land(req).await.map_err(st)?.into_inner();
        Ok((Id::from_slice(&r.landed)?, r.rebased))
    }

    async fn log(&self, rev: &str, path: &str, limit: u32) -> Result<Vec<RemoteChangeset>> {
        let req = LogRequest { rev: rev.to_owned(), path: path.to_owned(), limit };
        let mut stream = HistoryClient::new(self.ch()).log(req).await.map_err(st)?.into_inner();
        let mut out = Vec::new();
        while let Some(info) = stream.message().await.map_err(st)? {
            out.push(verify(info)?);
        }
        Ok(out)
    }

    async fn diff(&self, from: &str, to: &str) -> Result<Vec<FileChange>> {
        let mut c = DiffClient::new(self.ch()).max_decoding_message_size(MAX_MESSAGE_BYTES);
        let r = c.diff(DiffRequest { from: from.to_owned(), to: to.to_owned() }).await.map_err(st)?.into_inner();
        Ok(r.changes.into_iter().map(|c| FileChange { path: c.path, blob: c.blob, mode: c.mode }).collect())
    }

    async fn merge_base(&self, a: &str, b: &str) -> Result<Option<Id>> {
        let req = MergeBaseRequest { a: a.to_owned(), b: b.to_owned() };
        let r = MergeBaseClient::new(self.ch()).merge_base(req).await.map_err(st)?.into_inner();
        if r.found { Ok(Some(Id::from_slice(&r.id)?)) } else { Ok(None) }
    }

    async fn list_dir(&self, rev: &str, path: &str) -> Result<Vec<DirEntry>> {
        let req = BrowseRequest { rev: rev.to_owned(), path: path.to_owned() };
        let r = BrowseClient::new(self.ch()).list_dir(req).await.map_err(st)?.into_inner();
        r.entries
            .into_iter()
            .map(|e| {
                validate_name(&e.name)?;
                Ok(DirEntry {
                    name: e.name,
                    mode: Mode::parse(e.mode)?,
                    id: if e.id.is_empty() { None } else { Some(Id::from_slice(&e.id)?) },
                })
            })
            .collect()
    }

    async fn read_file(&self, rev: &str, path: &str) -> Result<(Mode, Vec<u8>)> {
        let req = BrowseRequest { rev: rev.to_owned(), path: path.to_owned() };
        let mut c = BrowseClient::new(self.ch()).max_decoding_message_size(MAX_MESSAGE_BYTES);
        let r = c.read_file(req).await.map_err(st)?.into_inner();
        let id = Id::from_slice(&r.id)?;
        if grovedock_core::Id::hash(grovedock_core::object::Kind::Blob as u8, &r.content) != id {
            bail!("server returned content that does not match {id}");
        }
        Ok((Mode::parse(r.mode)?, r.content))
    }

    async fn find(&self, rev: &str, pattern: &str, limit: u32) -> Result<Vec<String>> {
        let req = FindRequest { rev: rev.to_owned(), pattern: pattern.to_owned(), limit };
        let mut c = BrowseClient::new(self.ch()).max_decoding_message_size(MAX_MESSAGE_BYTES);
        let r = c.find(req).await.map_err(st)?.into_inner();
        if r.truncated {
            eprintln!("warning: results truncated");
        }
        Ok(r.paths)
    }

    async fn git_ai_note(&self, change: Id) -> Result<Option<String>> {
        let r = HistoryClient::new(self.ch())
            .git_ai_note(grovedock_proto::IdMsg { id: change.0.to_vec() })
            .await
            .map_err(st)?;
        let note = r.into_inner().note;
        Ok((!note.is_empty()).then_some(note))
    }

    async fn blame(&self, rev: &str, path: &str) -> Result<Vec<(Id, String, String)>> {
        let req = BlameRequest { rev: rev.to_owned(), path: path.to_owned() };
        let mut c = HistoryClient::new(self.ch()).max_decoding_message_size(MAX_MESSAGE_BYTES);
        let r = c.blame(req).await.map_err(st)?.into_inner();
        r.lines
            .into_iter()
            .map(|l| {
                Ok((Id::from_slice(&l.changeset)?, l.text, r.authors.get(&l.by).map(who_wrote).unwrap_or_default()))
            })
            .collect()
    }

    async fn lock(&self, path: &str) -> Result<(String, String, i64)> {
        let l = LocksClient::new(self.ch()).lock(LockRequest { path: path.to_owned() }).await.map_err(st)?.into_inner();
        Ok((l.path, l.owner, l.locked_at_ms))
    }

    async fn unlock(&self, path: &str, force: bool) -> Result<()> {
        LocksClient::new(self.ch()).unlock(UnlockRequest { path: path.to_owned(), force }).await.map_err(st)?;
        Ok(())
    }

    async fn locks(&self, prefix: &str) -> Result<Vec<(String, String, i64)>> {
        let r = LocksClient::new(self.ch()).list(ListLocksRequest { prefix: prefix.to_owned() }).await.map_err(st)?;
        Ok(r.into_inner().locks.into_iter().map(|l| (l.path, l.owner, l.locked_at_ms)).collect())
    }

    async fn whoami(&self) -> Result<ReadScope> {
        let r = AccessClient::new(self.ch()).who_am_i(Empty {}).await.map_err(st)?.into_inner();
        Ok(if r.full_read { ReadScope::All } else { ReadScope::Prefixes(r.read_prefixes) })
    }

    async fn checkout(
        &self,
        rev: &str,
        paths: &[String],
        sink: &mut (dyn FnMut(Piece) -> Result<()> + Send),
    ) -> Result<()> {
        let req = CheckoutRequest { rev: rev.to_owned(), paths: paths.to_vec() };
        let mut c = BrowseClient::new(self.ch()).max_decoding_message_size(MAX_MESSAGE_BYTES);
        let mut stream = c.checkout(req).await.map_err(st)?.into_inner();
        while let Some(p) = stream.message().await.map_err(st)? {
            validate_path(&p.path)?;
            sink(Piece {
                mode: Mode::parse(p.mode)?,
                id: Id::from_slice(&p.id)?,
                path: p.path,
                data: p.data,
                last: p.last,
            })?;
        }
        Ok(())
    }
}

/// How blame names a line's writer: an agent with its model ("claude_code · claude-sonnet-4"),
/// or a person.
pub fn who_wrote(a: &grovedock_proto::LineAuthor) -> String {
    if a.agent && !a.model.is_empty() { format!("{} · {}", a.name, a.model) } else { a.name.clone() }
}
