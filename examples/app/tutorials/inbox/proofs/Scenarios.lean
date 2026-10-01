import Noninterference
/-!
# Small concrete runs

The theorems would hold trivially for a kernel that refused everything or a
`view` that showed everything. These runs rule both out: messages arrive,
blocks refuse, and a third user cannot tell two different states apart.
-/
open Aeneas Aeneas.Std Result inbox_kernel inbox_kernel.Spec inbox_kernel.Commands
  inbox_kernel.Noninterference H5iAppLib

namespace inbox_kernel.Scenarios

def vec {α} (l : List α) (h : l.length ≤ Usize.max := by simp only [List.length_cons, List.length_nil]; scalar_tac) :
    alloc.vec.Vec α := alloc.vec.Vec.from l h

def alice : Principal := ⟨1#u64, 1#u64⟩
def bob : Principal := ⟨1#u64, 2#u64⟩
def carol : Principal := ⟨1#u64, 3#u64⟩

/-- "hi" -/
def hi : alloc.vec.Vec U8 := vec [104#u8, 105#u8]

/-- Alice's first message to Bob. -/
def msg0 : Message := ⟨1#u64, 2#u64, 0#u64, hi, false, false, false⟩

def empty : Snapshot := ⟨vec [], vec []⟩
/-- Alice has sent Bob one message. -/
def sent : Snapshot := ⟨vec [msg0], vec []⟩
/-- Bob blocks Alice. -/
def blocked : Snapshot := ⟨vec [], vec [⟨2#u64, 1#u64⟩]⟩

theorem text_ok_ok (t : alloc.vec.Vec U8) : text_ok t = ok (decide (0 < t.length ∧ t.length ≤ 1000)) := by
  obtain ⟨b, hb, hbt⟩ := (WP.spec_equiv_exists _ _).1 (text_ok_spec t)
  rw [hb]
  congr 1
  cases b <;> simp_all [textOk]

attribute [local simp] vec alloc.vec.Vec.from_val alice bob carol hi msg0 empty sent blocked
  u8vec_clone text_ok_ok is_blocked_ok last_seq_ok find_message_ok one_eq blockedBy keyIs lastSeq lastStep

/-- Alice can send to Bob; the message gets `seq` 0. -/
theorem alice_sends :
    transition alice empty (.Send 2#u64 hi) = ok (.Ok (vec [.PutMessage msg0], .Sent 0#u64)) := by
  simp [transition, send]

/-- Bob sees Alice's message in his inbox. -/
theorem bob_reads :
    ∃ v, transition bob sent .Inbox = ok (.Ok (alloc.vec.Vec.new Write, .Messages v)) ∧ v.val = [msg0] := by
  obtain ⟨v, e, h⟩ := Theorems.mailbox_ok sent.messages bob.user true
  refine ⟨v, by simp only [transition, e, bind_ok], ?_⟩
  rw [h]; simp [kept]

/-- Carol's inbox is empty, and she cannot delete the message. -/
theorem carol_sees_nothing :
    ∃ v, transition carol sent .Inbox = ok (.Ok (alloc.vec.Vec.new Write, .Messages v)) ∧ v.val = [] := by
  obtain ⟨v, e, h⟩ := Theorems.mailbox_ok sent.messages carol.user true
  refine ⟨v, by simp only [transition, e, bind_ok], ?_⟩
  rw [h]; simp [kept]

theorem carol_cannot_delete :
    transition carol sent (.Delete 1#u64 2#u64 0#u64) = ok (.Err .NotFound) := by
  simp [transition, delete]

/-- Bob can delete it from his inbox; Alice keeps it in her sent box. -/
theorem bob_deletes :
    transition bob sent (.Delete 1#u64 2#u64 0#u64) =
      ok (.Ok (vec [.PutMessage { msg0 with recipient_deleted := true }], .Done)) := by
  simp [transition, delete]

/-- Once Bob blocks Alice, her messages to him are refused, and she is told. -/
theorem block_refuses :
    transition alice blocked (.Send 2#u64 hi) = ok (.Err .Blocked) := by
  simp [transition, send]

/-- The block affects only Alice's messages to Bob. -/
theorem block_is_narrow :
    transition alice blocked (.Send 3#u64 hi) =
      ok (.Ok (vec [.PutMessage ⟨1#u64, 3#u64, 0#u64, hi, false, false, false⟩], .Sent 0#u64)) := by
  simp [transition, send]

/-- Carol's view of the two states is the same although they differ, so no
command of hers can tell them apart. -/
theorem carol_cannot_tell :
    empty ≠ sent ∧ ∀ c, transition carol empty c = transition carol sent c := by
  refine ⟨fun h => ?_, fun c => noninterference carol empty sent c ?_⟩
  · have := congrArg (fun s => s.messages.val) h
    simp at this
  · simp [view, Snapshot.toSt, involves]

/-- Alice's view does include Bob's block: it changes what she is told. -/
theorem alice_sees_block :
    view (Snapshot.toSt empty) 1 ≠ view (Snapshot.toSt blocked) 1 ∧
      transition alice empty (.Send 2#u64 hi) ≠ transition alice blocked (.Send 2#u64 hi) := by
  refine ⟨by simp [view, Snapshot.toSt], ?_⟩
  rw [alice_sends, block_refuses]
  simp

end inbox_kernel.Scenarios
