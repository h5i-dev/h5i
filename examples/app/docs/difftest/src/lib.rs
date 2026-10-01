//! Case generation and the text encoding shared with `proofs/DiffTest.lean`.
//! Both sides print numbers separated by spaces; keep the two in sync.

use docs_kernel::*;

pub struct Rng(pub u64);

impl Rng {
    pub fn below(&mut self, n: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) % n
    }
}

// ---- encoding (input and output use the same shapes) ----

fn bytes(out: &mut Vec<u64>, b: &[u8]) {
    out.push(b.len() as u64);
    out.extend(b.iter().map(|&x| x as u64));
}

fn role(r: Role) -> u64 {
    match r {
        Role::Viewer => 0,
        Role::Editor => 1,
        Role::Owner => 2,
    }
}

fn status(s: Status) -> u64 {
    match s {
        Status::Draft => 0,
        Status::InReview => 1,
        Status::Approved => 2,
        Status::Published => 3,
    }
}

fn project(out: &mut Vec<u64>, p: &Project) {
    out.push(p.id);
    bytes(out, &p.name);
}

fn member(out: &mut Vec<u64>, m: &Member) {
    out.extend([m.project, m.user, role(m.role)]);
}

fn doc(out: &mut Vec<u64>, d: &Document) {
    out.extend([d.id, d.project, d.author]);
    bytes(out, &d.title);
    bytes(out, &d.body);
    out.push(status(d.status));
    match d.approver {
        None => out.push(0),
        Some(a) => out.extend([1, a]),
    }
    out.push(d.version);
}

fn list<T>(out: &mut Vec<u64>, xs: &[T], f: fn(&mut Vec<u64>, &T)) {
    out.push(xs.len() as u64);
    xs.iter().for_each(|x| f(out, x));
}

fn hook(out: &mut Vec<u64>, w: &Webhook) {
    out.extend([w.project, w.dest]);
}

fn snapshot(out: &mut Vec<u64>, s: &Snapshot) {
    out.push(s.counter.next_id);
    list(out, &s.projects, project);
    list(out, &s.members, member);
    list(out, &s.documents, doc);
    list(out, &s.webhooks, hook);
}

fn command(out: &mut Vec<u64>, c: &Command) {
    match c {
        Command::CreateProject { name } => {
            out.push(0);
            bytes(out, name)
        }
        Command::SetMember {
            project,
            user,
            role: r,
        } => out.extend([1, *project, *user, role(*r)]),
        Command::RemoveMember { project, user } => out.extend([2, *project, *user]),
        Command::CreateDocument {
            project,
            title,
            body,
        } => {
            out.extend([3, *project]);
            bytes(out, title);
            bytes(out, body)
        }
        Command::EditDocument {
            doc,
            body,
            expected_version,
        } => {
            out.extend([4, *doc]);
            bytes(out, body);
            out.push(*expected_version)
        }
        Command::Submit { doc } => out.extend([5, *doc]),
        Command::Approve { doc } => out.extend([6, *doc]),
        Command::Publish { doc } => out.extend([7, *doc]),
        Command::DeleteDocument { doc } => out.extend([8, *doc]),
        Command::GetDocument { doc } => out.extend([9, *doc]),
        Command::ListDocuments { project } => out.extend([10, *project]),
        Command::SetWebhook { project, dest } => {
            out.extend([11, *project]);
            match dest {
                None => out.push(0),
                Some(d) => out.extend([1, *d]),
            }
        }
    }
}

fn write(out: &mut Vec<u64>, w: &Write) {
    match w {
        Write::PutProject(p) => {
            out.push(0);
            project(out, p)
        }
        Write::PutMember(m) => {
            out.push(1);
            member(out, m)
        }
        Write::DelMember(p, u) => out.extend([2, *p, *u]),
        Write::PutDocument(d) => {
            out.push(3);
            doc(out, d)
        }
        Write::DelDocument(i) => out.extend([4, *i]),
        Write::SetCounter(c) => out.extend([5, c.next_id]),
        Write::PutWebhook(w) => {
            out.push(6);
            hook(out, w)
        }
        Write::DelWebhook(p) => out.extend([7, *p]),
        Write::Emit(e) => out.extend([8, e.dest, e.project, e.doc, e.version]),
    }
}

fn reply(out: &mut Vec<u64>, r: &Reply) {
    match r {
        Reply::Created(i) => out.extend([0, *i]),
        Reply::Done => out.push(1),
        Reply::Version(v) => out.extend([2, *v]),
        Reply::Doc(d) => {
            out.push(3);
            doc(out, d)
        }
        Reply::Docs(ds) => {
            out.push(4);
            list(out, ds, doc)
        }
    }
}

fn error(e: Error) -> u64 {
    match e {
        Error::NotFound => 0,
        Error::Forbidden => 1,
        Error::Conflict => 2,
        Error::BadState => 3,
        Error::LastOwner => 4,
        Error::SelfApproval => 5,
        Error::Overflow => 6,
    }
}

fn join(v: &[u64]) -> String {
    v.iter().map(u64::to_string).collect::<Vec<_>>().join(" ")
}

/// One input line for the Lean executable.
pub fn encode_case(a: &Principal, s: &Snapshot, c: &Command) -> String {
    let mut out = vec![a.org, a.user];
    snapshot(&mut out, s);
    command(&mut out, c);
    join(&out)
}

/// What the Rust kernel does, in the output format of `DiffTest.runLine`.
pub fn run_rust(a: &Principal, s: &Snapshot, c: &Command) -> String {
    let r = std::panic::catch_unwind(|| match transition(a, s, c) {
        Err(e) => format!("ERR {}", error(e)),
        Ok((ws, rep)) => {
            let after = apply(s, &ws);
            let mut w = vec![];
            list(&mut w, &ws, write);
            let mut r = vec![];
            reply(&mut r, &rep);
            let mut n = vec![];
            snapshot(&mut n, &after);
            format!("OK {} | {} | {}", join(&w), join(&r), join(&n))
        }
    });
    r.unwrap_or_else(|_| "FAIL".into())
}

// ---- generation ----

fn text(r: &mut Rng) -> Text {
    (0..r.below(4)).map(|_| b'a' + r.below(26) as u8).collect()
}

fn pick_role(r: &mut Rng) -> Role {
    [Role::Viewer, Role::Editor, Role::Owner][r.below(3) as usize]
}

pub fn random_command(r: &mut Rng) -> Command {
    command_in(r, &Snapshot::default())
}

/// Mostly ids that exist in `s`, so commands get past the lookups.
pub fn command_in(r: &mut Rng, s: &Snapshot) -> Command {
    let pids: Vec<u64> = s.projects.iter().map(|p| p.id).collect();
    let dids: Vec<u64> = s.documents.iter().map(|d| d.id).collect();
    let docs: Vec<&Document> = s.documents.iter().collect();
    let from = |r: &mut Rng, xs: &[u64]| {
        if xs.is_empty() || r.below(4) == 0 {
            r.below(10)
        } else {
            xs[r.below(xs.len() as u64) as usize]
        }
    };
    let id = |r: &mut Rng| from(r, &dids);
    let pid = |r: &mut Rng| from(r, &pids);
    let user = |r: &mut Rng| 1 + r.below(4);
    let version = |r: &mut Rng| match docs.get(r.below(docs.len().max(1) as u64) as usize) {
        Some(d) if r.below(3) > 0 => d.version,
        _ => 1 + r.below(4),
    };
    match r.below(12) {
        0 => Command::CreateProject { name: text(r) },
        1 => Command::SetMember {
            project: pid(r),
            user: user(r),
            role: pick_role(r),
        },
        2 => Command::RemoveMember {
            project: pid(r),
            user: user(r),
        },
        3 => Command::CreateDocument {
            project: pid(r),
            title: text(r),
            body: text(r),
        },
        4 => Command::EditDocument {
            doc: id(r),
            body: text(r),
            expected_version: version(r),
        },
        5 => Command::Submit { doc: id(r) },
        6 => Command::Approve { doc: id(r) },
        7 => Command::Publish { doc: id(r) },
        8 => Command::DeleteDocument { doc: id(r) },
        9 => Command::GetDocument { doc: id(r) },
        10 => Command::ListDocuments { project: pid(r) },
        _ => Command::SetWebhook { project: pid(r), dest: if r.below(3) == 0 { None } else { Some(r.below(3)) } },
    }
}

pub fn random_actor(r: &mut Rng) -> Principal {
    Principal {
        org: 1,
        user: 1 + r.below(4),
    }
}

/// Mostly an existing member, so permission checks pass.
pub fn actor_in(r: &mut Rng, s: &Snapshot) -> Principal {
    match s
        .members
        .get(r.below(s.members.len().max(1) as u64) as usize)
    {
        Some(m) if r.below(4) > 0 => Principal {
            org: 1,
            user: m.user,
        },
        _ => random_actor(r),
    }
}

/// A reachable state, built by running random commands through the Rust kernel.
pub fn reachable(r: &mut Rng) -> Snapshot {
    let mut s = Snapshot::default();
    for _ in 0..r.below(60) {
        let (a, c) = (actor_in(r, &s), command_in(r, &s));
        if let Ok((ws, _)) = transition(&a, &s, &c) {
            s = apply(&s, &ws);
        }
    }
    s
}

/// Breaks invariants and pushes numbers to their limits; the kernel must
/// still agree with its translation on such states.
pub fn perturb(r: &mut Rng, s: &mut Snapshot) {
    for _ in 0..1 + r.below(3) {
        match r.below(6) {
            0 => s.counter.next_id = u64::MAX - r.below(2),
            1 => {
                if let Some(d) = s.documents.first_mut() {
                    d.version = u64::MAX - r.below(2);
                }
            }
            2 => {
                if let Some(m) = s.members.first().cloned() {
                    s.members.push(Member {
                        role: pick_role(r),
                        ..m
                    });
                }
            }
            3 => s.documents.push(Document {
                id: r.below(10),
                project: r.below(10),
                author: 1 + r.below(4),
                title: text(r),
                body: text(r),
                status: [
                    Status::Draft,
                    Status::InReview,
                    Status::Approved,
                    Status::Published,
                ][r.below(4) as usize],
                approver: if r.below(2) == 0 {
                    None
                } else {
                    Some(1 + r.below(4))
                },
                version: r.below(5),
            }),
            4 => s.members.push(Member {
                project: r.below(10),
                user: 1 + r.below(4),
                role: pick_role(r),
            }),
            _ => s.projects.push(Project {
                id: r.below(10),
                name: text(r),
            }),
        }
    }
}
