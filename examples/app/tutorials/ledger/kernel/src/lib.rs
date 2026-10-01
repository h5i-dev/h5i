//! Tutorial 3: a ledger of accounts owned by the users of an organization.
//!
//! A user opens accounts, deposits into and withdraws from their own
//! accounts, and transfers from their own accounts to any account. Balances
//! are `u64` and never go negative. The `ledger` row records the next account
//! id and the total ever deposited and withdrawn.

/// Who is calling: an organization (the tenant) and a user in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Principal {
    pub org: u64,
    pub user: u64,
}

h5i_app_schema::schema! {
    mapping ledger_tables for ledger_kernel, writes Write, lean "../proofs/generated/Schema.lean";

    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct Snapshot {
        ledger: Ledger,
        accounts: Vec<Account>,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Account in "accounts" {
        key { id: u64 }
        owner: u64,
        balance: u64,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
    pub struct Ledger in "ledger" {
        key {}
        next_id: u64,
        deposited: u64,
        withdrawn: u64,
    }
}

/// One organization's state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Open,
    Deposit { account: u64, amount: u64 },
    Withdraw { account: u64, amount: u64 },
    Transfer { src: u64, dst: u64, amount: u64 },
    List,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Write {
    PutAccount(Account),
    SetLedger(Ledger),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Created(u64),
    /// The balance of the account the command debited or credited.
    Balance(u64),
    Accounts(Vec<Account>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    NotFound,
    Forbidden,
    Insufficient,
    SameAccount,
    Overflow,
}

type Outcome = Result<(Vec<Write>, Reply), Error>;

fn two(a: Write, b: Write) -> Vec<Write> {
    let mut ws = Vec::new();
    ws.push(a);
    ws.push(b);
    ws
}

pub fn find_account(accounts: &Vec<Account>, id: u64) -> Option<Account> {
    let mut i = 0;
    while i < accounts.len() {
        if accounts[i].id == id {
            return Some(accounts[i]);
        }
        i += 1;
    }
    None
}

fn open(user: u64, s: &Snapshot) -> Outcome {
    let l = s.ledger;
    if l.next_id == u64::MAX {
        return Err(Error::Overflow);
    }
    let a = Account { id: l.next_id, owner: user, balance: 0 };
    let l2 = Ledger { next_id: l.next_id + 1, deposited: l.deposited, withdrawn: l.withdrawn };
    Ok((two(Write::PutAccount(a), Write::SetLedger(l2)), Reply::Created(a.id)))
}

fn deposit(user: u64, s: &Snapshot, id: u64, amount: u64) -> Outcome {
    match find_account(&s.accounts, id) {
        None => Err(Error::NotFound),
        Some(a) => {
            let l = s.ledger;
            if a.owner != user {
                Err(Error::Forbidden)
            } else if l.deposited > u64::MAX - amount {
                // Total deposits must fit in a u64.
                Err(Error::Overflow)
            } else if a.balance > u64::MAX - amount {
                Err(Error::Overflow)
            } else {
                let a2 = Account { id, owner: a.owner, balance: a.balance + amount };
                let l2 = Ledger { next_id: l.next_id, deposited: l.deposited + amount, withdrawn: l.withdrawn };
                Ok((two(Write::PutAccount(a2), Write::SetLedger(l2)), Reply::Balance(a2.balance)))
            }
        }
    }
}

fn withdraw(user: u64, s: &Snapshot, id: u64, amount: u64) -> Outcome {
    match find_account(&s.accounts, id) {
        None => Err(Error::NotFound),
        Some(a) => {
            let l = s.ledger;
            if a.owner != user {
                Err(Error::Forbidden)
            } else if amount > a.balance {
                Err(Error::Insufficient)
            } else if l.withdrawn > u64::MAX - amount {
                Err(Error::Overflow)
            } else {
                let a2 = Account { id, owner: a.owner, balance: a.balance - amount };
                let l2 = Ledger { next_id: l.next_id, deposited: l.deposited, withdrawn: l.withdrawn + amount };
                Ok((two(Write::PutAccount(a2), Write::SetLedger(l2)), Reply::Balance(a2.balance)))
            }
        }
    }
}

fn transfer(user: u64, s: &Snapshot, src: u64, dst: u64, amount: u64) -> Outcome {
    if src == dst {
        return Err(Error::SameAccount);
    }
    match find_account(&s.accounts, src) {
        None => Err(Error::NotFound),
        Some(a) => {
            if a.owner != user {
                Err(Error::Forbidden)
            } else if amount > a.balance {
                Err(Error::Insufficient)
            } else {
                match find_account(&s.accounts, dst) {
                    None => Err(Error::NotFound),
                    Some(b) => {
                        if b.balance > u64::MAX - amount {
                            Err(Error::Overflow)
                        } else {
                            let a2 = Account { id: src, owner: a.owner, balance: a.balance - amount };
                            let b2 = Account { id: dst, owner: b.owner, balance: b.balance + amount };
                            Ok((two(Write::PutAccount(a2), Write::PutAccount(b2)), Reply::Balance(a2.balance)))
                        }
                    }
                }
            }
        }
    }
}

/// The app logic: a command's writes and reply, or a refusal that commits
/// nothing.
pub fn transition(actor: &Principal, s: &Snapshot, cmd: &Command) -> Outcome {
    match cmd {
        Command::Open => open(actor.user, s),
        Command::Deposit { account, amount } => deposit(actor.user, s, *account, *amount),
        Command::Withdraw { account, amount } => withdraw(actor.user, s, *account, *amount),
        Command::Transfer { src, dst, amount } => transfer(actor.user, s, *src, *dst, *amount),
        Command::List => Ok((Vec::new(), Reply::Accounts(s.accounts.clone()))),
    }
}

/// What one write does to the state; `schema!` builds `apply` from it.
fn apply_write(s: &mut Snapshot, w: Write) {
    match w {
        Write::PutAccount(a) => Account::put(&mut s.accounts, a),
        Write::SetLedger(l) => s.ledger = l,
    }
}

/// The table writes one write makes.
fn sql_write(w: &Write, out: &mut Vec<h5i_app_sql::Write>) {
    match w {
        Write::PutAccount(a) => out.push(a.sql_put()),
        Write::SetLedger(l) => out.push(l.sql_put()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(s: &Snapshot, user: u64, c: Command) -> (Snapshot, Result<Reply, Error>) {
        match transition(&Principal { org: 1, user }, s, &c) {
            Ok((ws, r)) => (apply(s, &ws), Ok(r)),
            Err(e) => (s.clone(), Err(e)),
        }
    }

    fn total(s: &Snapshot) -> u64 {
        s.accounts.iter().map(|a| a.balance).sum()
    }

    #[test]
    fn deposits_transfers_and_refusals() {
        let s = Snapshot::default();
        let (s, r) = run(&s, 1, Command::Open);
        assert_eq!(r, Ok(Reply::Created(0)));
        let (s, _) = run(&s, 2, Command::Open);
        let (s, r) = run(&s, 1, Command::Deposit { account: 0, amount: 100 });
        assert_eq!(r, Ok(Reply::Balance(100)));
        assert_eq!(run(&s, 2, Command::Withdraw { account: 0, amount: 1 }).1, Err(Error::Forbidden));
        assert_eq!(run(&s, 2, Command::Transfer { src: 0, dst: 1, amount: 1 }).1, Err(Error::Forbidden));
        assert_eq!(run(&s, 1, Command::Transfer { src: 0, dst: 0, amount: 1 }).1, Err(Error::SameAccount));
        assert_eq!(run(&s, 1, Command::Withdraw { account: 0, amount: 101 }).1, Err(Error::Insufficient));
        let (s, r) = run(&s, 1, Command::Transfer { src: 0, dst: 1, amount: 30 });
        assert_eq!(r, Ok(Reply::Balance(70)));
        let (s, r) = run(&s, 2, Command::Withdraw { account: 1, amount: 10 });
        assert_eq!(r, Ok(Reply::Balance(20)));
        assert_eq!(total(&s) + s.ledger.withdrawn, s.ledger.deposited);
        assert_eq!(run(&s, 1, Command::Deposit { account: 0, amount: u64::MAX }).1, Err(Error::Overflow));
    }
}
