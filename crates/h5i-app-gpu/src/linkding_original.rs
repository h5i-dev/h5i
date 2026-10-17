//! Copied from h5i-app-bench performance/linkding/rust-h5i/kernel/src/search.rs.
//! Parser and independent host oracle; URL routines omitted.
#![allow(unused_assignments)]
//! Search, URL validation and paging, from `../../rust-h5i-optimized/kernel`.
//!
//! `parse_search` and `bookmark_matches` are linkding's query language
//! (terms, `#tags`, `!unread`, `!untagged`, `and` / `or` / `not`, quotes),
//! compared with ASCII case-folding. `valid_url` is Django's `URLValidator`
//! for the schemes linkding accepts. `page_of` is the paging of the list
//! endpoints. The kernel runs the first two on the rows it loads.

pub const NODE_TERM: u8 = 1;
pub const NODE_TAG: u8 = 2;
pub const NODE_KEYWORD: u8 = 3;
pub const NODE_AND: u8 = 4;
pub const NODE_OR: u8 = 5;
pub const NODE_NOT: u8 = 6;
const OP_LPAREN: u8 = 7;
const TOK_RPAREN: u8 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Page {
    pub limit: u64,
    pub offset: u64,
    pub has_next: bool,
    pub has_prev: bool,
    pub prev_drop_offset: bool,
    pub prev_offset: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub kind: u8,
    pub text: Vec<u8>,
    pub left: usize,
    pub right: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parsed {
    pub nodes: Vec<Node>,
    pub root: usize,
    pub failed: bool,
    pub empty: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BookmarkFacts {
    pub title: Vec<u8>,
    pub description: Vec<u8>,
    pub notes: Vec<u8>,
    pub url: Vec<u8>,
    pub unread: bool,
    pub untagged: bool,
    pub tag_names: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Token {
    kind: u8,
    text: Vec<u8>,
}

/// `limit_bad` covers a missing, empty, non-numeric, zero, or negative limit.
/// `offset_bad` covers a non-numeric or negative offset. A missing offset is
/// `offset_bad = false` and `offset = 0`.
pub fn page_of(limit_bad: bool, limit: u64, offset_bad: bool, offset: u64, count: u64) -> Page {
    let mut limit_n = limit;
    if limit_bad || limit == 0 {
        limit_n = 100;
    }
    let mut offset_n = offset;
    if offset_bad {
        offset_n = 0;
    }
    let mut has_next = false;
    if offset_n < count {
        let rest = count - offset_n;
        if rest > limit_n {
            has_next = true;
        }
    }
    let mut has_prev = false;
    let mut prev_drop_offset = false;
    let mut prev_offset = 0;
    if offset_n > 0 {
        has_prev = true;
        if offset_n <= limit_n {
            prev_drop_offset = true;
        } else {
            prev_offset = offset_n - limit_n;
        }
    }
    Page {
        limit: limit_n,
        offset: offset_n,
        has_next,
        has_prev,
        prev_drop_offset,
        prev_offset,
    }
}

fn empty_parsed() -> Parsed {
    Parsed {
        nodes: Vec::new(),
        root: 0,
        failed: false,
        empty: true,
    }
}

fn failed_parsed() -> Parsed {
    Parsed {
        nodes: Vec::new(),
        root: 0,
        failed: true,
        empty: false,
    }
}

pub fn lower_ascii(c: u8) -> u8 {
    match c {
        b'A' => b'a',
        b'B' => b'b',
        b'C' => b'c',
        b'D' => b'd',
        b'E' => b'e',
        b'F' => b'f',
        b'G' => b'g',
        b'H' => b'h',
        b'I' => b'i',
        b'J' => b'j',
        b'K' => b'k',
        b'L' => b'l',
        b'M' => b'm',
        b'N' => b'n',
        b'O' => b'o',
        b'P' => b'p',
        b'Q' => b'q',
        b'R' => b'r',
        b'S' => b's',
        b'T' => b't',
        b'U' => b'u',
        b'V' => b'v',
        b'W' => b'w',
        b'X' => b'x',
        b'Y' => b'y',
        b'Z' => b'z',
        _ => c,
    }
}

pub fn is_space(c: u8) -> bool {
    c == b' ' || c == b'\t' || c == b'\n' || c == b'\r' || c == 0x0b || c == 0x0c
}

fn trim_ascii(query: &Vec<u8>) -> Vec<u8> {
    let mut start = 0;
    while start < query.len() && is_space(query[start]) {
        start += 1;
    }
    let mut end = query.len();
    while end > start {
        let prev = end - 1;
        if is_space(query[prev]) {
            end = prev;
        } else {
            break;
        }
    }
    let mut out: Vec<u8> = Vec::new();
    let mut i = start;
    while i < end {
        out.push(query[i]);
        i += 1;
    }
    out
}

fn push_copy(out: &mut Vec<u8>, src: &Vec<u8>, from: usize, to: usize) {
    let mut i = from;
    while i < to && i < src.len() {
        out.push(src[i]);
        i += 1;
    }
}

fn is_and(text: &Vec<u8>) -> bool {
    if text.len() != 3 {
        return false;
    }
    lower_ascii(text[0]) == b'a' && lower_ascii(text[1]) == b'n' && lower_ascii(text[2]) == b'd'
}

fn is_or(text: &Vec<u8>) -> bool {
    if text.len() != 2 {
        return false;
    }
    lower_ascii(text[0]) == b'o' && lower_ascii(text[1]) == b'r'
}

fn is_not_word(text: &Vec<u8>) -> bool {
    if text.len() != 3 {
        return false;
    }
    lower_ascii(text[0]) == b'n' && lower_ascii(text[1]) == b'o' && lower_ascii(text[2]) == b't'
}

fn tokenize(query: &Vec<u8>) -> Vec<Token> {
    let mut tokens: Vec<Token> = Vec::new();
    let mut i = 0;
    while i < query.len() {
        while i < query.len() && is_space(query[i]) {
            i += 1;
        }
        if i >= query.len() {
            break;
        }
        let c = query[i];
        if c == b'(' {
            tokens.push(Token {
                kind: OP_LPAREN,
                text: Vec::new(),
            });
            i += 1;
        } else if c == b')' {
            tokens.push(Token {
                kind: TOK_RPAREN,
                text: Vec::new(),
            });
            i += 1;
        } else if c == b'"' || c == b'\'' {
            let quote = c;
            i += 1;
            let mut text: Vec<u8> = Vec::new();
            while i < query.len() && query[i] != quote {
                if query[i] == b'\\' {
                    i += 1;
                    if i >= query.len() {
                        break;
                    }
                    let esc = query[i];
                    if esc == b'n' {
                        text.push(b'\n');
                    } else if esc == b't' {
                        text.push(b'\t');
                    } else if esc == b'r' {
                        text.push(b'\r');
                    } else if esc == b'\\' {
                        text.push(b'\\');
                    } else if esc == quote {
                        text.push(quote);
                    } else {
                        text.push(esc);
                    }
                    i += 1;
                } else {
                    text.push(query[i]);
                    i += 1;
                }
            }
            if i < query.len() && query[i] == quote {
                i += 1;
            }
            tokens.push(Token {
                kind: NODE_TERM,
                text,
            });
        } else if c == b'#' || c == b'!' {
            let marked = c;
            i += 1;
            let from = i;
            while i < query.len()
                && !is_space(query[i])
                && query[i] != b'('
                && query[i] != b')'
                && query[i] != b'"'
                && query[i] != b'\''
            {
                i += 1;
            }
            if i > from {
                let mut text: Vec<u8> = Vec::new();
                push_copy(&mut text, query, from, i);
                let kind = if marked == b'#' { NODE_TAG } else { NODE_KEYWORD };
                tokens.push(Token { kind, text });
            }
        } else {
            let from = i;
            while i < query.len()
                && !is_space(query[i])
                && query[i] != b'('
                && query[i] != b')'
                && query[i] != b'"'
                && query[i] != b'\''
                && query[i] != b'#'
                && query[i] != b'!'
            {
                i += 1;
            }
            if i > from {
                let mut text: Vec<u8> = Vec::new();
                push_copy(&mut text, query, from, i);
                let kind = if is_and(&text) {
                    NODE_AND
                } else if is_or(&text) {
                    NODE_OR
                } else if is_not_word(&text) {
                    NODE_NOT
                } else {
                    NODE_TERM
                };
                let keep = if kind == NODE_TERM { text } else { Vec::new() };
                tokens.push(Token { kind, text: keep });
            }
        }
    }
    tokens
}

fn prec(op: u8) -> u8 {
    if op == NODE_OR {
        1
    } else if op == NODE_AND {
        2
    } else if op == NODE_NOT {
        3
    } else {
        0
    }
}

fn leaf(kind: u8, text: Vec<u8>) -> Node {
    Node {
        kind,
        text,
        left: 0,
        right: 0,
    }
}

fn push_usize(stack: &mut Vec<usize>, len: &mut usize, item: usize) {
    if *len < stack.len() {
        stack[*len] = item;
    } else {
        stack.push(item);
    }
    *len = *len + 1;
}

fn pop_usize(stack: &Vec<usize>, len: &mut usize) -> usize {
    if *len == 0 {
        return 0;
    }
    *len = *len - 1;
    if *len < stack.len() {
        stack[*len]
    } else {
        0
    }
}

fn push_u8(stack: &mut Vec<u8>, len: &mut usize, item: u8) {
    if *len < stack.len() {
        stack[*len] = item;
    } else {
        stack.push(item);
    }
    *len = *len + 1;
}

fn top_u8(stack: &Vec<u8>, len: usize) -> u8 {
    if len > 0 && len - 1 < stack.len() {
        stack[len - 1]
    } else {
        0
    }
}

fn apply_op(
    op: u8,
    values: &mut Vec<usize>,
    values_len: &mut usize,
    nodes: &mut Vec<Node>,
    failed: &mut bool,
) {
    if op == NODE_NOT {
        if *values_len == 0 {
            *failed = true;
            return;
        }
        let left = pop_usize(values, values_len);
        nodes.push(Node {
            kind: NODE_NOT,
            text: Vec::new(),
            left,
            right: 0,
        });
        push_usize(values, values_len, nodes.len() - 1);
        return;
    }
    if *values_len < 2 {
        *failed = true;
        return;
    }
    let right = pop_usize(values, values_len);
    let left = pop_usize(values, values_len);
    nodes.push(Node {
        kind: op,
        text: Vec::new(),
        left,
        right,
    });
    push_usize(values, values_len, nodes.len() - 1);
}

fn push_op(
    ops: &mut Vec<u8>,
    ops_len: &mut usize,
    values: &mut Vec<usize>,
    values_len: &mut usize,
    nodes: &mut Vec<Node>,
    op: u8,
    failed: &mut bool,
) {
    while *ops_len > 0 && !*failed {
        let top = top_u8(ops, *ops_len);
        if top == OP_LPAREN {
            break;
        }
        let top_p = prec(top);
        let op_p = prec(op);
        if top_p > op_p || (top_p == op_p && op != NODE_NOT) {
            *ops_len = *ops_len - 1;
            apply_op(top, values, values_len, nodes, failed);
        } else {
            break;
        }
    }
    if !*failed {
        push_u8(ops, ops_len, op);
    }
}

fn parse_tokens(tokens: &Vec<Token>) -> Parsed {
    let mut nodes: Vec<Node> = Vec::new();
    let mut values: Vec<usize> = Vec::new();
    let mut values_len: usize = 0;
    let mut ops: Vec<u8> = Vec::new();
    let mut ops_len: usize = 0;
    let mut expect_operand = true;
    let mut failed = false;
    let mut i = 0;
    while i < tokens.len() && !failed {
        let kind = tokens[i].kind;
        if kind == NODE_TERM || kind == NODE_TAG || kind == NODE_KEYWORD {
            if !expect_operand {
                push_op(
                    &mut ops,
                    &mut ops_len,
                    &mut values,
                    &mut values_len,
                    &mut nodes,
                    NODE_AND,
                    &mut failed,
                );
            }
            if !failed {
                let mut text: Vec<u8> = Vec::new();
                push_copy(&mut text, &tokens[i].text, 0, tokens[i].text.len());
                nodes.push(leaf(kind, text));
                push_usize(&mut values, &mut values_len, nodes.len() - 1);
                expect_operand = false;
            }
        } else if kind == NODE_AND || kind == NODE_OR {
            if expect_operand {
                failed = true;
            } else {
                push_op(
                    &mut ops,
                    &mut ops_len,
                    &mut values,
                    &mut values_len,
                    &mut nodes,
                    kind,
                    &mut failed,
                );
                expect_operand = true;
            }
        } else if kind == NODE_NOT {
            if !expect_operand {
                push_op(
                    &mut ops,
                    &mut ops_len,
                    &mut values,
                    &mut values_len,
                    &mut nodes,
                    NODE_AND,
                    &mut failed,
                );
            }
            if !failed {
                push_op(
                    &mut ops,
                    &mut ops_len,
                    &mut values,
                    &mut values_len,
                    &mut nodes,
                    NODE_NOT,
                    &mut failed,
                );
                expect_operand = true;
            }
        } else if kind == OP_LPAREN {
            if !expect_operand {
                push_op(
                    &mut ops,
                    &mut ops_len,
                    &mut values,
                    &mut values_len,
                    &mut nodes,
                    NODE_AND,
                    &mut failed,
                );
            }
            if !failed {
                push_u8(&mut ops, &mut ops_len, OP_LPAREN);
                expect_operand = true;
            }
        } else if kind == TOK_RPAREN {
            if expect_operand {
                failed = true;
            } else {
                let mut saw = false;
                while ops_len > 0 && !failed {
                    let top = top_u8(&ops, ops_len);
                    if top == OP_LPAREN {
                        saw = true;
                        break;
                    }
                    ops_len = ops_len - 1;
                    apply_op(top, &mut values, &mut values_len, &mut nodes, &mut failed);
                }
                if !saw || ops_len == 0 {
                    failed = true;
                } else {
                    ops_len = ops_len - 1;
                    expect_operand = false;
                }
            }
        } else {
            failed = true;
        }
        i += 1;
    }
    if expect_operand {
        failed = true;
    }
    while ops_len > 0 && !failed {
        let top = top_u8(&ops, ops_len);
        if top == OP_LPAREN {
            failed = true;
            break;
        }
        ops_len = ops_len - 1;
        apply_op(top, &mut values, &mut values_len, &mut nodes, &mut failed);
    }
    if failed || values_len != 1 || values.len() == 0 {
        return failed_parsed();
    }
    Parsed {
        nodes,
        root: values[0],
        failed: false,
        empty: false,
    }
}

/// Empty or whitespace-only input matches every bookmark. A parse error
/// matches nothing (`failed` is set).
pub fn parse_search(query: &Vec<u8>) -> Parsed {
    if query.len() == 0 {
        return empty_parsed();
    }
    let trimmed = trim_ascii(query);
    if trimmed.len() == 0 {
        return empty_parsed();
    }
    let tokens = tokenize(&trimmed);
    if tokens.len() == 0 {
        return empty_parsed();
    }
    parse_tokens(&tokens)
}

fn node_kind(nodes: &Vec<Node>, i: usize) -> u8 {
    if i < nodes.len() {
        nodes[i].kind
    } else {
        0
    }
}

fn node_left(nodes: &Vec<Node>, i: usize) -> usize {
    if i < nodes.len() {
        nodes[i].left
    } else {
        0
    }
}

fn node_right(nodes: &Vec<Node>, i: usize) -> usize {
    if i < nodes.len() {
        nodes[i].right
    } else {
        0
    }
}

fn node_text_len(nodes: &Vec<Node>, i: usize) -> usize {
    if i < nodes.len() {
        nodes[i].text.len()
    } else {
        0
    }
}

fn node_text_at(nodes: &Vec<Node>, i: usize, j: usize) -> u8 {
    if i < nodes.len() && j < nodes[i].text.len() {
        nodes[i].text[j]
    } else {
        0
    }
}

fn contains_node(hay: &Vec<u8>, nodes: &Vec<Node>, node: usize) -> bool {
    let nlen = node_text_len(nodes, node);
    if nlen == 0 {
        return true;
    }
    if nlen > hay.len() {
        return false;
    }
    let mut found = false;
    let mut i = 0;
    while i < hay.len() && !found {
        if hay.len() - i < nlen {
            i = hay.len();
        } else {
            let mut j = 0;
            let mut ok = true;
            while j < nlen {
                let idx = i + j;
                if idx < hay.len() {
                    if lower_ascii(hay[idx]) != lower_ascii(node_text_at(nodes, node, j)) {
                        ok = false;
                    }
                } else {
                    ok = false;
                }
                j += 1;
            }
            if ok {
                found = true;
            }
            i += 1;
        }
    }
    found
}

fn eq_node(hay: &Vec<u8>, nodes: &Vec<Node>, node: usize) -> bool {
    let nlen = node_text_len(nodes, node);
    if hay.len() != nlen {
        return false;
    }
    let mut i = 0;
    while i < nlen {
        if lower_ascii(hay[i]) != lower_ascii(node_text_at(nodes, node, i)) {
            return false;
        }
        i += 1;
    }
    true
}

fn any_tag(bm: &BookmarkFacts, nodes: &Vec<Node>, node: usize) -> bool {
    let mut i = 0;
    while i < bm.tag_names.len() {
        if eq_node(&bm.tag_names[i], nodes, node) {
            return true;
        }
        i += 1;
    }
    false
}

fn is_word(nodes: &Vec<Node>, node: usize, word: &Vec<u8>) -> bool {
    let nlen = node_text_len(nodes, node);
    if nlen != word.len() {
        return false;
    }
    let mut i = 0;
    while i < nlen {
        if lower_ascii(node_text_at(nodes, node, i)) != lower_ascii(word[i]) {
            return false;
        }
        i += 1;
    }
    true
}

pub fn unread_word() -> Vec<u8> {
    let mut word: Vec<u8> = Vec::new();
    word.push(b'u');
    word.push(b'n');
    word.push(b'r');
    word.push(b'e');
    word.push(b'a');
    word.push(b'd');
    word
}

pub fn untagged_word() -> Vec<u8> {
    let mut word: Vec<u8> = Vec::new();
    word.push(b'u');
    word.push(b'n');
    word.push(b't');
    word.push(b'a');
    word.push(b'g');
    word.push(b'g');
    word.push(b'e');
    word.push(b'd');
    word
}

fn keyword_matches(nodes: &Vec<Node>, node: usize, bm: &BookmarkFacts) -> bool {
    let unread = unread_word();
    if is_word(nodes, node, &unread) {
        return bm.unread;
    }
    let untagged = untagged_word();
    if is_word(nodes, node, &untagged) {
        return bm.untagged;
    }
    true
}

fn term_matches(nodes: &Vec<Node>, node: usize, lax: bool, bm: &BookmarkFacts) -> bool {
    if contains_node(&bm.title, nodes, node)
        || contains_node(&bm.description, nodes, node)
        || contains_node(&bm.notes, nodes, node)
        || contains_node(&bm.url, nodes, node)
    {
        return true;
    }
    if lax {
        return any_tag(bm, nodes, node);
    }
    false
}

fn eval_parsed(parsed: &Parsed, lax: bool, bm: &BookmarkFacts) -> bool {
    let mut vals: Vec<bool> = Vec::new();
    let mut i = 0;
    while i < parsed.nodes.len() {
        let kind = node_kind(&parsed.nodes, i);
        let left = node_left(&parsed.nodes, i);
        let right = node_right(&parsed.nodes, i);
        let mut value = false;
        if kind == NODE_TERM {
            value = term_matches(&parsed.nodes, i, lax, bm);
        } else if kind == NODE_TAG {
            value = any_tag(bm, &parsed.nodes, i);
        } else if kind == NODE_KEYWORD {
            value = keyword_matches(&parsed.nodes, i, bm);
        } else if kind == NODE_AND {
            if left < vals.len() && right < vals.len() {
                value = vals[left] && vals[right];
            } else {
                return false;
            }
        } else if kind == NODE_OR {
            if left < vals.len() && right < vals.len() {
                value = vals[left] || vals[right];
            } else {
                return false;
            }
        } else if kind == NODE_NOT {
            if left < vals.len() {
                value = !vals[left];
            } else {
                return false;
            }
        } else {
            return false;
        }
        vals.push(value);
        i += 1;
    }
    if parsed.root < vals.len() {
        vals[parsed.root]
    } else {
        false
    }
}

/// `failed` queries match nothing. An empty query matches every bookmark.
/// Otherwise the bookmark satisfies the parsed term, tag, keyword, and
/// boolean structure. `lax` also treats a term as an exact tag name.
pub fn bookmark_matches(parsed: &Parsed, lax: bool, bm: &BookmarkFacts) -> bool {
    if parsed.failed {
        return false;
    }
    if parsed.empty {
        return true;
    }
    if parsed.nodes.len() == 0 {
        return false;
    }
    eval_parsed(parsed, lax, bm)
}

