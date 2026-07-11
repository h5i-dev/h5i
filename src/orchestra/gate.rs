//! Durable human-in-the-loop gate. Split out of `mod.rs` by concern.

use super::*;


/// A human's reply to a [`Conductor::gate`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateAnswer {
    pub from: String,
    pub body: String,
}

impl GateAnswer {
    /// Approval convention: first token `APPROVE`/`APPROVED`/`LGTM`/`YES`.
    pub fn approved(&self) -> bool {
        first_token_approves(&self.body)
    }
}

/// A pending durable question to a human (design doc §4.3). Two journaled
/// steps: the ask (records the sent message id, so a resume never re-asks) and
/// the wait (records the reply). A score that times out waiting can simply
/// exit; re-running it resumes the wait on the already-delivered question. The
/// human answers with `h5i msg reply <n> APPROVE …` (or any text).
pub struct Gate {
    core: Arc<Core>,
    question: String,
    to: Option<String>,
}

impl Conductor {
    /// Ask a human a durable question. Default recipient is the score's actor
    /// identity (the question lands in their `h5i msg` inbox); override with
    /// [`Gate::to`].
    pub fn gate(&self, question: impl Into<String>) -> Gate {
        Gate {
            core: self.core.clone(),
            question: question.into(),
            to: None,
        }
    }
}

impl Gate {
    /// Address the question to a specific agent/human identity.
    pub fn to(mut self, recipient: impl Into<String>) -> Self {
        self.to = Some(recipient.into());
        self
    }

    /// Resolve to `true` when the reply approves (see [`GateAnswer::approved`]).
    pub async fn approve(self) -> Result<bool, H5iError> {
        Ok(self.answer().await?.approved())
    }

    /// Resolve to the full reply.
    pub async fn answer(self) -> Result<GateAnswer, H5iError> {
        let Gate { core, question, to } = self;
        let recipient = to.unwrap_or_else(|| core.actor.clone());

        // Step 1 — deliver the question once. The journaled result is the sent
        // message id; a resume replays it and never re-asks.
        let ask_core = core.clone();
        let (ask_q, ask_to) = (question.clone(), recipient.clone());
        let msg_id: String = journaled(core.clone(), "gate_ask".into(), move |c| {
            let repo = c.repo()?;
            let body = format!(
                "[gate] {ask_q}\n\n(h5i orchestra run '{run}': a score is paused on your \
                 answer — reply with `h5i msg reply <n> APPROVE` or `DECLINE <reason>`; \
                 re-running the score resumes from this gate.)",
                run = c.run_id,
            );
            let message = msg::send_msg(
                &repo,
                &c.h5i_root,
                &c.actor,
                &ask_to,
                &body,
                msg::SendOpts {
                    kind: Some("ASK".into()),
                    priority: Some("high".into()),
                    links: Some(serde_json::json!({
                        "team": c.run_id,
                        "gate": true,
                    })),
                    ..Default::default()
                },
            )?;
            let ev = team::event(
                &c.run_id,
                &c.actor,
                "orch_gate_asked",
                0,
                None,
                None,
                format!("orch_gate_asked:{}:{}", c.run_id, message.id),
                serde_json::json!({ "to": ask_to, "question": ask_q, "message_id": message.id }),
            );
            team::append_event(&repo, &ev)?;
            Ok(message.id)
        })
        .await?;

        // Step 2 — wait for the reply. The label embeds the message id, so the
        // ask/wait pairing is stable under any concurrency or resume order.
        let wait_label = format!("gate_wait/{msg_id}");
        journaled(ask_core, wait_label, move |c| {
            wait_until(
                c,
                &format!("a reply to gate message {msg_id} (from {recipient})"),
                |repo| {
                    let reply = msg::read_messages(repo)
                        .into_iter()
                        .filter(|m| m.reply_to.as_deref() == Some(msg_id.as_str()))
                        .max_by(|a, b| (a.ts.as_str(), a.id.as_str()).cmp(&(b.ts.as_str(), b.id.as_str())));
                    Ok(reply.map(|m| GateAnswer {
                        from: m.from,
                        body: m.body,
                    }))
                },
            )
        })
        .await
    }
}

