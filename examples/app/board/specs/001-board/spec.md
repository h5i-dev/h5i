# Feature Specification: Bulletin board

**Feature Branch**: `001-board`

**Created**: 2026-11-15

**Status**: Implemented

**Input**: User description: "An organization's bulletin board. Everyone in
the organization reads and publishes posts. Only the author edits a post; the
author or a moderator deletes it. Moderators appoint and remove moderators,
and the board never loses its last moderator."

This spec was written after the board was built, to show the h5i Spec Kit
preset on an application whose proofs already exist.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Publish and read posts (Priority: P1)

A member of an organization publishes a short text post and sees every post
of the organization.

**Why this priority**: Without posts there is no board.

**Independent Test**: Publish a post as one user and list the posts as
another user of the same organization.

**Acceptance Scenarios**:

1. **Given** an empty board, **When** Alice publishes "hi", **Then** the board
   has one post with id 0, author Alice and text "hi".
2. **Given** any board, **When** a user publishes an empty text or one longer
   than 280 bytes, **Then** the post is refused and nothing changes.

---

### User Story 2 - Edit and delete posts (Priority: P2)

Authors correct their own posts. Authors and moderators remove posts.

**Why this priority**: A board needs corrections and moderation, but works
without them.

**Independent Test**: Try an edit and a delete as the author, as another
member and as a moderator.

**Acceptance Scenarios**:

1. **Given** Alice's post, **When** Bob edits it, **Then** the edit is refused
   and the post is unchanged.
2. **Given** Alice's post and Bob is not a moderator, **When** Bob deletes it,
   **Then** the delete is refused.
3. **Given** Alice's post and Carol is a moderator, **When** Carol deletes it,
   **Then** the post is gone.

---

### User Story 3 - Moderators (Priority: P3)

The first user to claim it becomes a moderator. Moderators appoint and remove
other moderators.

**Why this priority**: Moderation depends on posts existing first.

**Independent Test**: On a fresh board, appoint a first moderator, appoint a
second, remove one, then try to remove the last.

**Acceptance Scenarios**:

1. **Given** a board with no moderator, **When** Alice appoints herself,
   **Then** Alice is a moderator.
2. **Given** Alice is the only moderator, **When** she removes herself,
   **Then** the command is refused with `LastModerator`.

---

### Edge Cases

- A post id at the top of the `u64` range: the counter refuses with
  `Overflow` instead of wrapping.
- Appointing someone who is already a moderator leaves one entry for them.
- Editing or deleting a post id that does not exist returns `NotFound`.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: Users MUST be able to publish a post of 1 to 280 bytes.
- **FR-002**: Users MUST be able to list every post of their organization.
- **FR-003**: Authors MUST be able to edit and delete their own posts.
- **FR-004**: Moderators MUST be able to delete any post.
- **FR-005**: Moderators MUST be able to appoint and remove moderators.
- **FR-006**: A user MUST be able to appoint themselves while the board has no
  moderator.

### Key Entities

- **Post**: id, author, text. Ids come from a per-organization counter.
- **Moderator**: a user of the organization who moderates the board.
- **Organization**: the tenant. Each organization has its own board.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: Every INV and AUTH rule below has a theorem that passes
  `h5i app prove`.
- **SC-002**: Every mutant in the plan is caught by `h5i app mutate`.

## Assumptions

- Read confidentiality is out of scope: every member reads every post of the
  organization.
- Rate limits and spam control are out of scope.

## Invariants and Authorization *(mandatory for h5i-app features)*

### State Invariants

- **INV-01**: Once a board has a moderator, it always has at least one.
  - **Broken when**: The last moderator's Demote is accepted.
- **INV-02**: A post's author never changes.
  - **Broken when**: Publish writes over an existing post of another author.
- **INV-03**: Post ids are unique and below the counter.
  - **Broken when**: Publish does not advance the counter, so the next post
    gets the same id.
- **INV-04**: Every stored post has 1 to 280 bytes.
  - **Broken when**: An empty post is accepted.
- **INV-05**: A user appears at most once among the moderators.
  - **Broken when**: Appointing an existing moderator adds a second entry.

### Authorization Rules

- **AUTH-01**: Only the author edits a post. A refused edit writes nothing.
  - **Broken when**: Bob's Edit of Alice's post is accepted.
- **AUTH-02**: Only the author or a moderator deletes a post.
  - **Broken when**: A member who is neither deletes another member's post.
- **AUTH-03**: Only a moderator appoints moderators, except that a user may
  appoint themselves while the board has none.
  - **Broken when**: A non-moderator appoints themselves on a board that
    already has a moderator.
- **AUTH-04**: Only a moderator removes a moderator.
  - **Broken when**: A non-moderator's Demote of a moderator is accepted.

### Trust Boundary

- The bearer token identifies the caller; token checking and the mapping from
  token to user are shell code.
- The organization in the token is the tenant; the shell loads that tenant's
  snapshot.
- JSON decoding, status codes and retry replies are shell code.
- PostgreSQL runs the rendered SQL as modeled, and the schema matches the
  model.
- Charon, Aeneas and Lean translate and check faithfully.
