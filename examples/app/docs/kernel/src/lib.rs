//! Kernel of the example app. Extracted to Lean by Aeneas (see `../proofs`).
//!
//! Aeneas subset: no `?`, iterator adapters, or `String`. Loops are `while`
//! over indices.

pub type Text = Vec<u8>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Principal {
    pub org: u64,
    pub user: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Viewer,
    Editor,
    Owner,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Read,
    Write,
    Approve,
    Manage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Draft,
    InReview,
    Approved,
    Published,
}

// Row types, declared once: the server derives its table mappings from this
// (`docs_kernel::docs_tables!`).
h5i_app_schema::schema! {
    mapping docs_tables for docs_kernel, lean "../proofs/generated/Schema.lean";

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Project in "projects" {
        key { id: u64 }
        name: Text,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Member in "members" {
        key { project: u64, user: u64 }
        role: Role,
    }

    #[derive(Debug, PartialEq, Eq)]
    pub struct Document in "documents" {
        key { id: u64 }
        project: u64,
        author: u64,
        title: Text,
        body: Text,
        status: Status,
        approver: Option<u64>,
        version: u64,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Default)]
    pub struct Counter in "counters" {
        key {}
        next_id: u64,
    }

    /// A project's notification target. `dest` names an entry in the
    /// operator's destination registry, never a URL, so users cannot point
    /// the server at a host of their choosing.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Webhook in "webhooks" {
        key { project: u64 }
        dest: u64,
    }
}

// Hand-written: the derived impl clones `Option<u64>`, which Aeneas models
// only as an axiom.
impl Clone for Document {
    fn clone(&self) -> Self {
        Document {
            id: self.id,
            project: self.project,
            author: self.author,
            title: self.title.clone(),
            body: self.body.clone(),
            status: self.status,
            approver: self.approver,
            version: self.version,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Snapshot {
    pub counter: Counter,
    pub projects: Vec<Project>,
    pub members: Vec<Member>,
    pub documents: Vec<Document>,
    pub webhooks: Vec<Webhook>,
}

/// A message to deliver outside the database, through the outbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Effect {
    pub dest: u64,
    pub project: u64,
    pub doc: u64,
    pub version: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Write {
    PutProject(Project),
    PutMember(Member),
    DelMember(u64, u64),
    PutDocument(Document),
    DelDocument(u64),
    SetCounter(Counter),
    PutWebhook(Webhook),
    DelWebhook(u64),
    Emit(Effect),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    CreateProject {
        name: Text,
    },
    SetMember {
        project: u64,
        user: u64,
        role: Role, // h5i-allow: privileged-field (owner action target)
    },
    RemoveMember {
        project: u64,
        user: u64,
    },
    CreateDocument {
        project: u64,
        title: Text,
        body: Text,
    },
    EditDocument {
        doc: u64,
        body: Text,
        expected_version: u64,
    },
    Submit {
        doc: u64,
    },
    Approve {
        doc: u64,
    },
    Publish {
        doc: u64,
    },
    DeleteDocument {
        doc: u64,
    },
    GetDocument {
        doc: u64,
    },
    ListDocuments {
        project: u64,
    },
    /// Set (`Some`) or clear (`None`) the project's webhook.
    SetWebhook {
        project: u64,
        dest: Option<u64>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Created(u64),
    Done,
    Version(u64),
    Doc(Document),
    Docs(Vec<Document>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    NotFound,
    Forbidden,
    Conflict,
    BadState,
    LastOwner,
    SelfApproval,
    Overflow,
}

/// The policy table. Proof obligations about it close by `decide`.
pub fn allows(role: Role, action: Action) -> bool {
    match role {
        Role::Owner => true,
        Role::Editor => match action {
            Action::Read => true,
            Action::Write => true,
            Action::Approve => false,
            Action::Manage => false,
        },
        Role::Viewer => match action {
            Action::Read => true,
            _ => false,
        },
    }
}

pub fn role_of(members: &Vec<Member>, project: u64, user: u64) -> Option<Role> {
    let mut i = 0;
    while i < members.len() {
        if members[i].project == project && members[i].user == user {
            return Some(members[i].role);
        }
        i += 1;
    }
    None
}

/// True if `user` may do `action` in `project`. Every command goes through this.
pub fn can(snap: &Snapshot, user: u64, project: u64, action: Action) -> bool {
    match role_of(&snap.members, project, user) {
        Some(role) => allows(role, action),
        None => false,
    }
}

pub fn project_exists(projects: &Vec<Project>, id: u64) -> bool {
    let mut i = 0;
    while i < projects.len() {
        if projects[i].id == id {
            return true;
        }
        i += 1;
    }
    false
}

pub fn find_document(docs: &Vec<Document>, id: u64) -> Option<Document> {
    let mut i = 0;
    while i < docs.len() {
        if docs[i].id == id {
            return Some(docs[i].clone());
        }
        i += 1;
    }
    None
}

pub fn count_owners(members: &Vec<Member>, project: u64) -> u64 {
    let mut n = 0;
    let mut i = 0;
    while i < members.len() {
        if members[i].project == project && members[i].role == Role::Owner {
            n += 1;
        }
        i += 1;
    }
    n
}

pub fn documents_in(docs: &Vec<Document>, project: u64) -> Vec<Document> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < docs.len() {
        if docs[i].project == project {
            out.push(docs[i].clone());
        }
        i += 1;
    }
    out
}

pub fn webhook_of(hooks: &Vec<Webhook>, project: u64) -> Option<u64> {
    let mut i = 0;
    while i < hooks.len() {
        if hooks[i].project == project {
            return Some(hooks[i].dest);
        }
        i += 1;
    }
    None
}

fn fresh_id(snap: &Snapshot) -> Result<(u64, Counter), Error> {
    let id = snap.counter.next_id;
    if id == u64::MAX {
        return Err(Error::Overflow);
    }
    Ok((id, Counter { next_id: id + 1 }))
}

/// Loads a document and checks `action` on its project. A caller who cannot
/// read the project gets `NotFound`, so ids of hidden documents do not leak.
fn authorized_doc(snap: &Snapshot, user: u64, doc: u64, action: Action) -> Result<Document, Error> {
    match find_document(&snap.documents, doc) {
        None => Err(Error::NotFound),
        Some(d) => {
            if !can(snap, user, d.project, Action::Read) {
                Err(Error::NotFound)
            } else if can(snap, user, d.project, action) {
                Ok(d)
            } else {
                Err(Error::Forbidden)
            }
        }
    }
}

fn one(w: Write) -> Vec<Write> {
    let mut v = Vec::new();
    v.push(w);
    v
}

fn with_status(d: Document, status: Status, approver: Option<u64>) -> Result<Document, Error> {
    if d.version == u64::MAX {
        return Err(Error::Overflow);
    }
    Ok(Document {
        id: d.id,
        project: d.project,
        author: d.author,
        title: d.title,
        body: d.body,
        status,
        approver,
        version: d.version + 1,
    })
}

/// What a command reads. The server loads only these rows; the proofs show
/// `transition` gives the same result on that slice as on the whole tenant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Only the id counter.
    Counter,
    /// The counter and every row of one project.
    Project(u64),
    /// The counter and every row of the project this document belongs to.
    Document(u64),
}

pub fn read_scope(cmd: &Command) -> Scope {
    match cmd {
        Command::CreateProject { .. } => Scope::Counter,
        Command::SetMember { project, .. } => Scope::Project(*project),
        Command::RemoveMember { project, .. } => Scope::Project(*project),
        Command::CreateDocument { project, .. } => Scope::Project(*project),
        Command::SetWebhook { project, .. } => Scope::Project(*project),
        Command::ListDocuments { project } => Scope::Project(*project),
        Command::EditDocument { doc, .. } => Scope::Document(*doc),
        Command::Submit { doc } => Scope::Document(*doc),
        Command::Approve { doc } => Scope::Document(*doc),
        Command::Publish { doc } => Scope::Document(*doc),
        Command::DeleteDocument { doc } => Scope::Document(*doc),
        Command::GetDocument { doc } => Scope::Document(*doc),
    }
}

pub fn transition(
    actor: &Principal,
    snap: &Snapshot,
    cmd: &Command,
) -> Result<(Vec<Write>, Reply), Error> {
    let user = actor.user;
    match cmd {
        Command::CreateProject { name } => {
            let (id, counter) = match fresh_id(snap) {
                Ok(v) => v,
                Err(e) => return Err(e),
            };
            let mut ws = Vec::new();
            ws.push(Write::SetCounter(counter));
            ws.push(Write::PutProject(Project {
                id,
                name: name.clone(),
            }));
            ws.push(Write::PutMember(Member {
                project: id,
                user,
                role: Role::Owner,
            }));
            Ok((ws, Reply::Created(id)))
        }
        Command::SetMember {
            project,
            user: target,
            role,
        } => {
            if !can(snap, user, *project, Action::Manage) {
                return Err(Error::Forbidden);
            }
            let target_is_owner = match role_of(&snap.members, *project, *target) {
                Some(Role::Owner) => true,
                _ => false,
            };
            let demotes_owner = *role != Role::Owner && target_is_owner;
            if demotes_owner && count_owners(&snap.members, *project) <= 1 {
                return Err(Error::LastOwner);
            }
            let m = Member {
                project: *project,
                user: *target,
                role: *role,
            };
            Ok((one(Write::PutMember(m)), Reply::Done))
        }
        Command::RemoveMember {
            project,
            user: target,
        } => {
            if !can(snap, user, *project, Action::Manage) {
                return Err(Error::Forbidden);
            }
            match role_of(&snap.members, *project, *target) {
                None => Err(Error::NotFound),
                Some(r) => {
                    if r == Role::Owner && count_owners(&snap.members, *project) <= 1 {
                        return Err(Error::LastOwner);
                    }
                    Ok((one(Write::DelMember(*project, *target)), Reply::Done))
                }
            }
        }
        Command::CreateDocument {
            project,
            title,
            body,
        } => {
            if !can(snap, user, *project, Action::Write) {
                return Err(Error::Forbidden);
            }
            let (id, counter) = match fresh_id(snap) {
                Ok(v) => v,
                Err(e) => return Err(e),
            };
            let d = Document {
                id,
                project: *project,
                author: user,
                title: title.clone(),
                body: body.clone(),
                status: Status::Draft,
                approver: None,
                version: 1,
            };
            let mut ws = Vec::new();
            ws.push(Write::SetCounter(counter));
            ws.push(Write::PutDocument(d));
            Ok((ws, Reply::Created(id)))
        }
        Command::EditDocument {
            doc,
            body,
            expected_version,
        } => {
            let d = match authorized_doc(snap, user, *doc, Action::Write) {
                Ok(v) => v,
                Err(e) => return Err(e),
            };
            if d.version != *expected_version {
                return Err(Error::Conflict);
            }
            if d.status == Status::Published {
                return Err(Error::BadState);
            }
            let mut e = match with_status(d, Status::Draft, None) {
                Ok(v) => v,
                Err(e) => return Err(e),
            };
            e.body = body.clone();
            let v = e.version;
            Ok((one(Write::PutDocument(e)), Reply::Version(v)))
        }
        Command::Submit { doc } => {
            let d = match authorized_doc(snap, user, *doc, Action::Write) {
                Ok(v) => v,
                Err(e) => return Err(e),
            };
            if d.status != Status::Draft {
                return Err(Error::BadState);
            }
            let e = match with_status(d, Status::InReview, None) {
                Ok(v) => v,
                Err(e) => return Err(e),
            };
            let v = e.version;
            Ok((one(Write::PutDocument(e)), Reply::Version(v)))
        }
        Command::Approve { doc } => {
            let d = match authorized_doc(snap, user, *doc, Action::Approve) {
                Ok(v) => v,
                Err(e) => return Err(e),
            };
            if d.status != Status::InReview {
                return Err(Error::BadState);
            }
            if d.author == user {
                return Err(Error::SelfApproval);
            }
            let e = match with_status(d, Status::Approved, Some(user)) {
                Ok(v) => v,
                Err(e) => return Err(e),
            };
            let v = e.version;
            Ok((one(Write::PutDocument(e)), Reply::Version(v)))
        }
        Command::Publish { doc } => {
            let d = match authorized_doc(snap, user, *doc, Action::Write) {
                Ok(v) => v,
                Err(e) => return Err(e),
            };
            if d.status != Status::Approved {
                return Err(Error::BadState);
            }
            let approver = d.approver;
            let e = match with_status(d, Status::Published, approver) {
                Ok(v) => v,
                Err(e) => return Err(e),
            };
            let v = e.version;
            let effect = match webhook_of(&snap.webhooks, e.project) {
                Some(dest) => Some(Effect {
                    dest,
                    project: e.project,
                    doc: e.id,
                    version: v,
                }),
                None => None,
            };
            let mut ws = one(Write::PutDocument(e));
            match effect {
                Some(eff) => ws.push(Write::Emit(eff)),
                None => {}
            }
            Ok((ws, Reply::Version(v)))
        }
        Command::DeleteDocument { doc } => {
            let d = match authorized_doc(snap, user, *doc, Action::Manage) {
                Ok(v) => v,
                Err(e) => return Err(e),
            };
            Ok((one(Write::DelDocument(d.id)), Reply::Done))
        }
        Command::GetDocument { doc } => {
            let d = match authorized_doc(snap, user, *doc, Action::Read) {
                Ok(v) => v,
                Err(e) => return Err(e),
            };
            Ok((Vec::new(), Reply::Doc(d)))
        }
        Command::SetWebhook { project, dest } => {
            if !can(snap, user, *project, Action::Manage) {
                return Err(Error::Forbidden);
            }
            let w = match dest {
                Some(d) => Write::PutWebhook(Webhook {
                    project: *project,
                    dest: *d,
                }),
                None => Write::DelWebhook(*project),
            };
            Ok((one(w), Reply::Done))
        }
        Command::ListDocuments { project } => {
            if !can(snap, user, *project, Action::Read) {
                return Err(Error::Forbidden);
            }
            Ok((
                Vec::new(),
                Reply::Docs(documents_in(&snap.documents, *project)),
            ))
        }
    }
}

fn put_project(v: &mut Vec<Project>, p: Project) {
    let mut i = 0;
    while i < v.len() {
        if v[i].id == p.id {
            v[i] = p;
            return;
        }
        i += 1;
    }
    v.push(p);
}

fn put_member(v: &mut Vec<Member>, m: Member) {
    let mut i = 0;
    while i < v.len() {
        if v[i].project == m.project && v[i].user == m.user {
            v[i] = m;
            return;
        }
        i += 1;
    }
    v.push(m);
}

fn put_document(v: &mut Vec<Document>, d: Document) {
    let mut i = 0;
    while i < v.len() {
        if v[i].id == d.id {
            v[i] = d;
            return;
        }
        i += 1;
    }
    v.push(d);
}

fn del_member(v: &Vec<Member>, project: u64, user: u64) -> Vec<Member> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < v.len() {
        if !(v[i].project == project && v[i].user == user) {
            out.push(v[i].clone());
        }
        i += 1;
    }
    out
}

fn del_document(v: &Vec<Document>, id: u64) -> Vec<Document> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < v.len() {
        if v[i].id != id {
            out.push(v[i].clone());
        }
        i += 1;
    }
    out
}

fn put_webhook(v: &mut Vec<Webhook>, w: Webhook) {
    let mut i = 0;
    while i < v.len() {
        if v[i].project == w.project {
            v[i] = w;
            return;
        }
        i += 1;
    }
    v.push(w);
}

fn del_webhook(v: &Vec<Webhook>, project: u64) -> Vec<Webhook> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < v.len() {
        if v[i].project != project {
            out.push(v[i]);
        }
        i += 1;
    }
    out
}

pub fn apply_write(s: &mut Snapshot, w: Write) {
    match w {
        Write::PutProject(p) => put_project(&mut s.projects, p),
        Write::PutMember(m) => put_member(&mut s.members, m),
        Write::DelMember(p, u) => s.members = del_member(&s.members, p, u),
        Write::PutDocument(d) => put_document(&mut s.documents, d),
        Write::DelDocument(id) => s.documents = del_document(&s.documents, id),
        Write::SetCounter(c) => s.counter = c,
        Write::PutWebhook(w) => put_webhook(&mut s.webhooks, w),
        Write::DelWebhook(p) => s.webhooks = del_webhook(&s.webhooks, p),
        Write::Emit(_) => {}
    }
}

// ---- Row encoding (A4) ----

// Must match the server's `enum_field!` numbering, which reads these back.
impl h5i_app_sql::Column for Role {
    fn to_val(&self) -> h5i_app_sql::Val {
        match self {
            Role::Viewer => h5i_app_sql::Val::Int(0),
            Role::Editor => h5i_app_sql::Val::Int(1),
            Role::Owner => h5i_app_sql::Val::Int(2),
        }
    }
    fn from_val(v: &h5i_app_sql::Val) -> Option<Role> {
        match v {
            h5i_app_sql::Val::Int(n) => {
                if *n == 0 {
                    Some(Role::Viewer)
                } else if *n == 1 {
                    Some(Role::Editor)
                } else if *n == 2 {
                    Some(Role::Owner)
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

impl h5i_app_sql::Column for Status {
    fn to_val(&self) -> h5i_app_sql::Val {
        match self {
            Status::Draft => h5i_app_sql::Val::Int(0),
            Status::InReview => h5i_app_sql::Val::Int(1),
            Status::Approved => h5i_app_sql::Val::Int(2),
            Status::Published => h5i_app_sql::Val::Int(3),
        }
    }
    fn from_val(v: &h5i_app_sql::Val) -> Option<Status> {
        match v {
            h5i_app_sql::Val::Int(n) => {
                if *n == 0 {
                    Some(Status::Draft)
                } else if *n == 1 {
                    Some(Status::InReview)
                } else if *n == 2 {
                    Some(Status::Approved)
                } else if *n == 3 {
                    Some(Status::Published)
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

fn put(table: u32, key_len: u32, row: Vec<h5i_app_sql::Val>) -> h5i_app_sql::Write {
    h5i_app_sql::Write::Put { table, key_len, row }
}

fn key1(a: u64) -> Vec<h5i_app_sql::Val> {
    let mut k = Vec::new();
    k.push(h5i_app_sql::Column::to_val(&a));
    k
}

fn key2(a: u64, b: u64) -> Vec<h5i_app_sql::Val> {
    let mut k = Vec::new();
    k.push(h5i_app_sql::Column::to_val(&a));
    k.push(h5i_app_sql::Column::to_val(&b));
    k
}

/// The table rows one write stores, or `None` for an outbox effect.
pub fn sql_write(w: &Write) -> Option<h5i_app_sql::Write> {
    match w {
        Write::PutProject(p) => Some(put(Project::TABLE, Project::KEY_LEN, p.to_row())),
        Write::PutMember(m) => Some(put(Member::TABLE, Member::KEY_LEN, m.to_row())),
        Write::DelMember(p, u) => Some(h5i_app_sql::Write::Del { table: Member::TABLE, key: key2(*p, *u) }),
        Write::PutDocument(d) => Some(put(Document::TABLE, Document::KEY_LEN, d.to_row())),
        Write::DelDocument(id) => Some(h5i_app_sql::Write::Del { table: Document::TABLE, key: key1(*id) }),
        Write::SetCounter(c) => Some(put(Counter::TABLE, Counter::KEY_LEN, c.to_row())),
        Write::PutWebhook(h) => Some(put(Webhook::TABLE, Webhook::KEY_LEN, h.to_row())),
        Write::DelWebhook(p) => Some(h5i_app_sql::Write::Del { table: Webhook::TABLE, key: key1(*p) }),
        Write::Emit(_) => None,
    }
}

/// The table writes of a write set, in order. The server plans and runs
/// exactly these; Lean proves they store what `apply` computes.
pub fn sql_writes(ws: &Vec<Write>) -> Vec<h5i_app_sql::Write> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < ws.len() {
        match sql_write(&ws[i]) {
            Some(w) => out.push(w),
            None => {}
        }
        i += 1;
    }
    out
}

/// A tenant's stored rows per table, in whatever order the database
/// returned them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rows {
    pub counter: Vec<Vec<h5i_app_sql::Val>>,
    pub projects: Vec<Vec<h5i_app_sql::Val>>,
    pub members: Vec<Vec<h5i_app_sql::Val>>,
    pub documents: Vec<Vec<h5i_app_sql::Val>>,
    pub webhooks: Vec<Vec<h5i_app_sql::Val>>,
}

/// The snapshot stored rows stand for. A tenant without a counter row has
/// counter 0. `None` if a row does not decode.
pub fn decode(r: &Rows) -> Option<Snapshot> {
    let counter = if r.counter.len() == 0 {
        Some(Counter { next_id: 0 })
    } else {
        Counter::from_row(&r.counter[0])
    };
    match (
        counter,
        Project::from_rows(&r.projects),
        Member::from_rows(&r.members),
        Document::from_rows(&r.documents),
        Webhook::from_rows(&r.webhooks),
    ) {
        (Some(counter), Some(projects), Some(members), Some(documents), Some(webhooks)) => {
            Some(Snapshot { counter, projects, members, documents, webhooks })
        }
        _ => None,
    }
}

/// The project whose rows a scoped load reads, if any. For a document
/// scope, `doc_rows` are the stored rows with that document's id.
pub fn scoped_project(scope: &Scope, doc_rows: &Vec<Vec<h5i_app_sql::Val>>) -> Option<u64> {
    match scope {
        Scope::Counter => None,
        Scope::Project(p) => Some(*p),
        Scope::Document(_) => match Document::from_rows(doc_rows) {
            Some(ds) => {
                if ds.len() > 0 {
                    Some(ds[0].project)
                } else {
                    None
                }
            }
            None => None,
        },
    }
}

/// Meaning of a write set. The Postgres store must agree with this.
pub fn apply(snap: &Snapshot, ws: &Vec<Write>) -> Snapshot {
    let mut s = snap.clone();
    let mut i = 0;
    while i < ws.len() {
        apply_write(&mut s, ws[i].clone());
        i += 1;
    }
    s
}

// ---- Invariant checker ----
//
// `check_inv` returns true exactly when the Lean `Inv` holds (proven in
// `proofs/Check.lean`). Migrations run it on every tenant before committing.
// Each check searches for a counterexample.

fn projects_owned(s: &Snapshot) -> bool {
    let mut i = 0;
    while i < s.projects.len() {
        if count_owners(&s.members, s.projects[i].id) == 0 {
            return false;
        }
        i += 1;
    }
    true
}

fn members_in_projects(s: &Snapshot) -> bool {
    let mut i = 0;
    while i < s.members.len() {
        if !project_exists(&s.projects, s.members[i].project) {
            return false;
        }
        i += 1;
    }
    true
}

fn docs_in_projects(s: &Snapshot) -> bool {
    let mut i = 0;
    while i < s.documents.len() {
        if !project_exists(&s.projects, s.documents[i].project) {
            return false;
        }
        i += 1;
    }
    true
}

fn hooks_in_projects(s: &Snapshot) -> bool {
    let mut i = 0;
    while i < s.webhooks.len() {
        if !project_exists(&s.projects, s.webhooks[i].project) {
            return false;
        }
        i += 1;
    }
    true
}

/// Is there a later project with the same id as `v[i]`?
fn project_dup_after(v: &Vec<Project>, i: usize) -> bool {
    let mut j = i + 1;
    while j < v.len() {
        if v[j].id == v[i].id {
            return true;
        }
        j += 1;
    }
    false
}

fn project_ids_unique(v: &Vec<Project>) -> bool {
    let mut i = 0;
    while i < v.len() {
        if project_dup_after(v, i) {
            return false;
        }
        i += 1;
    }
    true
}

fn member_dup_after(v: &Vec<Member>, i: usize) -> bool {
    let mut j = i + 1;
    while j < v.len() {
        if v[j].project == v[i].project && v[j].user == v[i].user {
            return true;
        }
        j += 1;
    }
    false
}

fn member_keys_unique(v: &Vec<Member>) -> bool {
    let mut i = 0;
    while i < v.len() {
        if member_dup_after(v, i) {
            return false;
        }
        i += 1;
    }
    true
}

fn doc_dup_after(v: &Vec<Document>, i: usize) -> bool {
    let mut j = i + 1;
    while j < v.len() {
        if v[j].id == v[i].id {
            return true;
        }
        j += 1;
    }
    false
}

fn doc_ids_unique(v: &Vec<Document>) -> bool {
    let mut i = 0;
    while i < v.len() {
        if doc_dup_after(v, i) {
            return false;
        }
        i += 1;
    }
    true
}

fn hook_dup_after(v: &Vec<Webhook>, i: usize) -> bool {
    let mut j = i + 1;
    while j < v.len() {
        if v[j].project == v[i].project {
            return true;
        }
        j += 1;
    }
    false
}

fn hook_projects_unique(v: &Vec<Webhook>) -> bool {
    let mut i = 0;
    while i < v.len() {
        if hook_dup_after(v, i) {
            return false;
        }
        i += 1;
    }
    true
}

fn projects_fresh(s: &Snapshot) -> bool {
    let mut i = 0;
    while i < s.projects.len() {
        if s.projects[i].id >= s.counter.next_id {
            return false;
        }
        i += 1;
    }
    true
}

fn docs_fresh(s: &Snapshot) -> bool {
    let mut i = 0;
    while i < s.documents.len() {
        if s.documents[i].id >= s.counter.next_id {
            return false;
        }
        i += 1;
    }
    true
}

/// Four-eyes rule and "approver present exactly when approved or published".
fn doc_well_formed(d: &Document) -> bool {
    let approved = match d.status {
        Status::Approved => true,
        Status::Published => true,
        _ => false,
    };
    match d.approver {
        Some(a) => approved && a != d.author,
        None => !approved,
    }
}

fn docs_well_formed(v: &Vec<Document>) -> bool {
    let mut i = 0;
    while i < v.len() {
        if !doc_well_formed(&v[i]) {
            return false;
        }
        i += 1;
    }
    true
}

pub fn check_inv(s: &Snapshot) -> bool {
    projects_owned(s)
        && members_in_projects(s)
        && docs_in_projects(s)
        && project_ids_unique(&s.projects)
        && member_keys_unique(&s.members)
        && doc_ids_unique(&s.documents)
        && projects_fresh(s)
        && docs_fresh(s)
        && docs_well_formed(&s.documents)
        && hooks_in_projects(s)
        && hook_projects_unique(&s.webhooks)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(s: &Snapshot, user: u64, cmd: Command) -> (Snapshot, Result<Reply, Error>) {
        match transition(&Principal { org: 1, user }, s, &cmd) {
            Ok((ws, r)) => (apply(s, &ws), Ok(r)),
            Err(e) => (s.clone(), Err(e)),
        }
    }

    #[test]
    fn reachable_states_pass_the_invariant_check() {
        let mut s = Snapshot::default();
        assert!(check_inv(&s));
        let mut r: u64 = 7;
        for _ in 0..2000 {
            r = r
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let pick = |k: u64| (r >> (33 + k)) % 6;
            let cmd = match pick(0) % 7 {
                0 => Command::CreateProject { name: vec![] },
                1 => Command::SetMember {
                    project: pick(1),
                    user: 1 + pick(2) % 3,
                    role: Role::Owner,
                },
                2 => Command::CreateDocument {
                    project: pick(1),
                    title: vec![],
                    body: vec![],
                },
                3 => Command::Submit { doc: pick(1) },
                4 => Command::Approve { doc: pick(1) },
                5 => Command::Publish { doc: pick(1) },
                _ => Command::SetWebhook {
                    project: pick(1),
                    dest: Some(1),
                },
            };
            let user = 1 + pick(3) % 3;
            if let Ok((ws, _)) = transition(&Principal { org: 1, user }, &s, &cmd) {
                s = apply(&s, &ws);
            }
            assert!(check_inv(&s), "{cmd:?}");
        }
        let mut broken = s.clone();
        broken.counter.next_id = 0;
        assert!(broken.projects.is_empty() || !check_inv(&broken));
    }

    #[test]
    fn outsiders_cannot_tell_hidden_documents_from_missing_ones() {
        let s = Snapshot::default();
        let (s, _) = run(&s, 1, Command::CreateProject { name: vec![] });
        let doc = Command::CreateDocument {
            project: 0,
            title: vec![],
            body: vec![],
        };
        let (s, r) = run(&s, 1, doc);
        assert_eq!(r, Ok(Reply::Created(1)));
        for cmd in [
            |d| Command::GetDocument { doc: d },
            |d| Command::Submit { doc: d },
            |d| Command::Approve { doc: d },
            |d| Command::DeleteDocument { doc: d },
        ] {
            let hidden = run(&s, 2, cmd(1)).1;
            let missing = run(&s, 2, cmd(99)).1;
            assert_eq!(hidden, missing);
        }
    }
}
