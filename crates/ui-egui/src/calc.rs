//! Calculations typed into number fields (#817): `100*2`, `1920/3`, `(50+10)*1.5`, and, applied to
//! the field's current value, `*2` or `/3`. Reads `+ - * /` (also `×` and `÷`), brackets, decimals
//! and signs; a unit (letters, `%`, `°`) and spaces are ignored. Anything else gives `None`, never a
//! panic: the text is capped, nesting is bounded, and only finite results come back.

/// Longest text read (characters).
const MAX_LEN: usize = 256;
/// Deepest nesting of brackets and signs.
const MAX_DEPTH: usize = 64;

/// The value of `text` typed into a field that holds `current`. A text starting with `*` or `/`
/// applies to `current` (`*2` doubles it); `+` and `-` at the start are signs.
pub fn eval(text: &str, current: f64) -> Option<f64> {
    let raw: Vec<char> = text.chars().collect();
    if raw.len() > MAX_LEN {
        return None;
    }
    // spaces or a unit between two numbers (`1e3`, `10x20`, `10 x 20`, `1920 1080`) would fuse them
    // into one number once dropped: refuse rather than guess
    let skipped = |c: &char| c.is_alphabetic() || c.is_whitespace() || matches!(c, '%' | '°');
    let mut after_number = false;
    let mut gap = false;
    for c in &raw {
        if skipped(c) {
            gap = true;
            continue;
        }
        let number_char = c.is_ascii_digit() || *c == '.';
        if number_char && after_number && gap {
            return None;
        }
        after_number = number_char;
        gap = false;
    }
    let chars: Vec<char> = raw
        .iter()
        .copied()
        .filter(|c| !skipped(c))
        .map(|c| match c {
            '×' => '*',
            '÷' => '/',
            c => c,
        })
        .collect();
    let mut p = Parser { chars: &chars, pos: 0 };
    let v = if matches!(chars.first(), Some('*' | '/')) {
        let t = p.term_from(current.is_finite().then_some(current)?, 0)?;
        p.expr_from(t, 0)?
    } else {
        p.expr(0)?
    };
    (p.pos == chars.len() && v.is_finite()).then_some(v)
}

struct Parser<'a> {
    chars: &'a [char],
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    /// expr := term (('+' | '-') term)*
    fn expr(&mut self, depth: usize) -> Option<f64> {
        let t = self.term(depth)?;
        self.expr_from(t, depth)
    }

    fn expr_from(&mut self, mut v: f64, depth: usize) -> Option<f64> {
        while let Some(op @ ('+' | '-')) = self.peek() {
            self.pos += 1;
            let r = self.term(depth)?;
            v = finite(if op == '+' { v + r } else { v - r })?;
        }
        Some(v)
    }

    /// term := factor (('*' | '/') factor)*
    fn term(&mut self, depth: usize) -> Option<f64> {
        let f = self.factor(depth)?;
        self.term_from(f, depth)
    }

    fn term_from(&mut self, mut v: f64, depth: usize) -> Option<f64> {
        while let Some(op @ ('*' | '/')) = self.peek() {
            self.pos += 1;
            let r = self.factor(depth)?;
            v = finite(if op == '*' { v * r } else { v / r })?;
        }
        Some(v)
    }

    /// factor := ('+' | '-') factor | number | '(' expr ')'
    fn factor(&mut self, depth: usize) -> Option<f64> {
        if depth >= MAX_DEPTH {
            return None;
        }
        match self.peek()? {
            '+' => {
                self.pos += 1;
                self.factor(depth + 1)
            }
            '-' => {
                self.pos += 1;
                self.factor(depth + 1).map(|v| -v)
            }
            '(' => {
                self.pos += 1;
                let v = self.expr(depth + 1)?;
                if self.peek()? != ')' {
                    return None;
                }
                self.pos += 1;
                Some(v)
            }
            _ => self.number(),
        }
    }

    fn number(&mut self) -> Option<f64> {
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_digit() || c == '.') {
            self.pos += 1;
        }
        let s: String = self.chars.get(start..self.pos)?.iter().collect();
        s.parse::<f64>().ok().and_then(finite)
    }
}

fn finite(v: f64) -> Option<f64> {
    v.is_finite().then_some(v)
}

#[cfg(test)]
mod tests {
    use super::eval;

    #[test]
    fn calculations() {
        assert_eq!(eval("100*2", 0.0), Some(200.0));
        assert_eq!(eval("1920/3", 0.0), Some(640.0));
        assert_eq!(eval("(50+10)*1.5", 0.0), Some(90.0));
        assert_eq!(eval("2+3*4", 0.0), Some(14.0), "* before +");
        assert_eq!(eval("10-2-3", 0.0), Some(5.0), "left to right");
        assert_eq!(eval("-(4+1)", 0.0), Some(-5.0));
        assert_eq!(eval(" .5 + 1. ", 0.0), Some(1.5));
        assert_eq!(eval("6×7", 0.0), Some(42.0));
        assert_eq!(eval("9÷3", 0.0), Some(3.0));
        assert_eq!(eval("42", 0.0), Some(42.0), "a plain number");
    }

    #[test]
    fn units_are_ignored() {
        assert_eq!(eval("50%*2", 0.0), Some(100.0));
        assert_eq!(eval("90°/2", 0.0), Some(45.0));
        assert_eq!(eval("1920 px / 2", 0.0), Some(960.0));
        assert_eq!(eval("-6 dB", 0.0), Some(-6.0));
    }

    #[test]
    fn a_leading_times_or_divide_applies_to_the_current_value() {
        assert_eq!(eval("*2", 150.0), Some(300.0));
        assert_eq!(eval("/3", 300.0), Some(100.0));
        assert_eq!(eval("/2+1", 10.0), Some(6.0));
        assert_eq!(eval("-10", 50.0), Some(-10.0), "a leading minus is a sign");
        assert_eq!(eval("+10", 50.0), Some(10.0), "a leading plus is a sign");
        // the field's text with an operation added after it
        assert_eq!(eval("200/3", 200.0), Some(200.0 / 3.0));
    }

    #[test]
    fn bad_input_gives_nothing() {
        for bad in ["", "   ", "abc", "2*", "*", "(1+2", "1+2)", "1..2", "1/0", "0/0", "2**3", "1,5", "()"] {
            assert_eq!(eval(bad, 7.0), None, "{bad:?}");
        }
        assert_eq!(eval("*2", f64::NAN), None);
        assert_eq!(eval(&"9".repeat(400), 0.0), None, "too long");
        assert_eq!(eval(&format!("{}1{}", "(".repeat(100), ")".repeat(100)), 0.0), None, "too deep");
        assert_eq!(eval(&format!("{}1", "-".repeat(100)), 0.0), None, "too many signs");
        // letters between digits are refused, not dropped (`1e3` is not 13)
        assert_eq!(eval("1e3", 0.0), None);
        assert_eq!(eval("10x20", 0.0), None);
    }

    #[test]
    fn numbers_separated_by_spaces_or_a_unit_are_not_merged() {
        for bad in ["10 x 20", "1 e 3", "1920 1080", "10px 20", "50% 2", "1 .5"] {
            assert_eq!(eval(bad, 0.0), None, "{bad:?}");
        }
        assert_eq!(eval("100 * 2", 0.0), Some(200.0));
        assert_eq!(eval("10px", 0.0), Some(10.0));
        assert_eq!(eval("10 px * 2", 0.0), Some(20.0));
        assert_eq!(eval("*2", 5.0), Some(10.0));
    }
}
