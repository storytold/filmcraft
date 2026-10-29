//! Column names from a `CREATE TABLE` statement (only as much SQL as that needs).

/// What a `CREATE TABLE` statement says about the stored rows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Columns {
    pub names: Vec<String>,
    /// The `INTEGER PRIMARY KEY` column, which is stored as the row id (its record value is NULL).
    pub rowid_alias: Option<usize>,
    /// Columns with REAL affinity: SQLite stores their integral values as integers.
    pub real: Vec<bool>,
    pub without_rowid: bool,
}

/// Table constraints start with one of these (unquoted) words; anything else is a column.
const CONSTRAINTS: [&str; 5] = ["CONSTRAINT", "PRIMARY", "UNIQUE", "CHECK", "FOREIGN"];

/// The columns of a `CREATE TABLE` statement (`None` when it cannot be parsed).
pub(crate) fn parse_create_table(sql: &str) -> Option<Columns> {
    let open = sql.find('(')?;
    let body_start = open + 1;
    let mut depth = 1usize;
    let mut parts = Vec::new();
    let mut part_start = body_start;
    let mut close = None;
    let mut quote: Option<char> = None;
    for (i, c) in sql[body_start..].char_indices() {
        let at = body_start + i;
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' | '`' => quote = Some(c),
            '[' => quote = Some(']'),
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    parts.push(&sql[part_start..at]);
                    close = Some(at);
                    break;
                }
            }
            ',' if depth == 1 => {
                parts.push(&sql[part_start..at]);
                part_start = at + 1;
            }
            _ => {}
        }
    }
    let close = close?;
    let mut cols = Columns { without_rowid: normalise(&sql[close + 1..]).contains("WITHOUT ROWID"), ..Default::default() };
    let mut table_pk: Option<Vec<String>> = None;
    for part in parts {
        let def = part.trim();
        let Some((name, quoted, rest)) = first_token(def) else { continue };
        if !quoted && CONSTRAINTS.iter().any(|k| name.eq_ignore_ascii_case(k)) {
            // ASCII upper-casing keeps every byte offset
            if let Some(i) = def.to_ascii_uppercase().find("PRIMARY") {
                table_pk = paren_list(&def[i..]);
            }
            continue;
        }
        let rest_n = normalise(rest);
        if rest_n.split(' ').next() == Some("INTEGER") && rest_n.contains("PRIMARY KEY") && !rest_n.contains("PRIMARY KEY DESC") {
            cols.rowid_alias = Some(cols.names.len());
        }
        cols.real.push(real_affinity(&declared_type(&rest_n)));
        cols.names.push(name);
    }
    // `PRIMARY KEY ("id")` as a table constraint makes a single INTEGER column the alias too.
    if cols.rowid_alias.is_none()
        && let Some(pk) = table_pk.filter(|pk| pk.len() == 1)
        && let Some(i) = cols.names.iter().position(|c| c.eq_ignore_ascii_case(&pk[0]))
        && declared_integer(sql, &cols.names[i])
    {
        cols.rowid_alias = Some(i);
    }
    if cols.without_rowid {
        cols.rowid_alias = None;
    }
    Some(cols)
}

/// The declared type: the normalised definition up to its first column constraint.
fn declared_type(rest: &str) -> String {
    const STOP: [&str; 10] = ["CONSTRAINT", "NOT", "NULL", "DEFAULT", "PRIMARY", "UNIQUE", "CHECK", "REFERENCES", "COLLATE", "GENERATED"];
    rest.split(' ').take_while(|w| !STOP.contains(w) && *w != "AS").collect::<Vec<_>>().join(" ")
}

/// SQLite's affinity rules, in order: INT wins, then text and blob types, then REAL.
fn real_affinity(t: &str) -> bool {
    !t.contains("INT")
        && !t.contains("CHAR")
        && !t.contains("CLOB")
        && !t.contains("TEXT")
        && !t.contains("BLOB")
        && !t.is_empty()
        && (t.contains("REAL") || t.contains("FLOA") || t.contains("DOUB"))
}

/// Upper-case with runs of whitespace collapsed to one space.
fn normalise(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_ascii_uppercase()
}

/// The identifiers in the first `( … )` of `s`.
fn paren_list(s: &str) -> Option<Vec<String>> {
    let open = s.find('(')?;
    let close = open + s[open..].find(')')?;
    Some(s[open + 1..close].split(',').filter_map(|p| first_token(p.trim()).map(|(n, ..)| n)).collect())
}

/// Whether the definition of column `name` has the declared type INTEGER.
fn declared_integer(sql: &str, name: &str) -> bool {
    let Some(cols_start) = sql.find('(') else { return false };
    sql[cols_start + 1..].split(',').any(|def| match first_token(def.trim()) {
        Some((n, _, rest)) => n.eq_ignore_ascii_case(name) && normalise(rest).split(' ').next() == Some("INTEGER"),
        None => false,
    })
}

/// The first identifier of a definition: (name, whether it was quoted, the text after it).
fn first_token(def: &str) -> Option<(String, bool, &str)> {
    let mut chars = def.char_indices();
    let (_, c) = chars.next()?;
    let close = match c {
        '"' => Some('"'),
        '`' => Some('`'),
        '[' => Some(']'),
        '\'' => Some('\''),
        _ => None,
    };
    if let Some(close) = close {
        let mut name = String::new();
        let mut it = def[1..].char_indices().peekable();
        while let Some((i, ch)) = it.next() {
            if ch == close {
                // a doubled quote is an escaped quote
                if close != ']' && it.peek().is_some_and(|(_, n)| *n == close) {
                    name.push(ch);
                    it.next();
                    continue;
                }
                return Some((name, true, &def[1 + i + ch.len_utf8()..]));
            }
            name.push(ch);
        }
        return None;
    }
    let end = def.find(|c: char| c.is_whitespace() || c == '(' || c == ',').unwrap_or(def.len());
    if end == 0 {
        return None;
    }
    Some((def[..end].to_string(), false, &def[end..]))
}
