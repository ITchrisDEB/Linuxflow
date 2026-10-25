//! Evaluator of the `plural=` expression of a gettext `Plural-Forms` header.
//!
//! C subset sufficient for all the usual catalogues: `n`, integers, `! * / % + -`,
//! comparisons, `&&`, `||` and `?:`. No panic: division by zero => 0.

#[derive(Debug, Clone, PartialEq)]
enum Expr {
    N,
    Num(i64),
    Not(Box<Expr>),
    Bin(Op, Box<Expr>, Box<Expr>),
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Op {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

/// Plural rule of a catalogue.
#[derive(Debug, Clone, PartialEq)]
pub struct PluralRule {
    nplurals: usize,
    expr: Expr,
}

impl Default for PluralRule {
    /// Germanic rule (English, old French...): `n != 1`.
    fn default() -> Self {
        PluralRule {
            nplurals: 2,
            expr: Expr::Bin(Op::Ne, Box::new(Expr::N), Box::new(Expr::Num(1))),
        }
    }
}

impl PluralRule {
    /// Parses the value of a `Plural-Forms` header (`nplurals=2; plural=(n > 1);`).
    pub fn parse(header: &str) -> Option<PluralRule> {
        let mut nplurals = None;
        let mut expr = None;
        for part in header.split(';') {
            let part = part.trim();
            if let Some(v) = part.strip_prefix("nplurals=") {
                nplurals = v.trim().parse::<usize>().ok();
            } else if let Some(v) = part.strip_prefix("plural=") {
                expr = parse_expr(v);
            }
        }
        let nplurals = nplurals.filter(|n| (1..=8).contains(n))?;
        Some(PluralRule {
            nplurals,
            expr: expr?,
        })
    }

    /// Number of forms.
    pub fn nplurals(&self) -> usize {
        self.nplurals
    }

    /// Index of the form for `n` (always < `nplurals`).
    pub fn index(&self, n: u64) -> usize {
        let v = eval(&self.expr, n.min(i64::MAX as u64) as i64);
        usize::try_from(v).unwrap_or(0).min(self.nplurals - 1)
    }
}

fn eval(e: &Expr, n: i64) -> i64 {
    match e {
        Expr::N => n,
        Expr::Num(v) => *v,
        Expr::Not(a) => i64::from(eval(a, n) == 0),
        Expr::Cond(c, a, b) => {
            if eval(c, n) != 0 {
                eval(a, n)
            } else {
                eval(b, n)
            }
        }
        Expr::Bin(op, a, b) => {
            // `&&` and `||` short-circuit as in C.
            match op {
                Op::Or => return i64::from(eval(a, n) != 0 || eval(b, n) != 0),
                Op::And => return i64::from(eval(a, n) != 0 && eval(b, n) != 0),
                _ => {}
            }
            let (x, y) = (eval(a, n), eval(b, n));
            match op {
                Op::Eq => i64::from(x == y),
                Op::Ne => i64::from(x != y),
                Op::Lt => i64::from(x < y),
                Op::Gt => i64::from(x > y),
                Op::Le => i64::from(x <= y),
                Op::Ge => i64::from(x >= y),
                Op::Add => x.wrapping_add(y),
                Op::Sub => x.wrapping_sub(y),
                Op::Mul => x.wrapping_mul(y),
                Op::Div => x.checked_div(y).unwrap_or(0),
                Op::Rem => x.checked_rem(y).unwrap_or(0),
                Op::Or | Op::And => 0,
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    N,
    Num(i64),
    Sym(&'static str),
}

fn lex(s: &str) -> Option<Vec<Tok>> {
    const SYMS: [&str; 17] = [
        "||", "&&", "==", "!=", "<=", ">=", "<", ">", "+", "-", "*", "/", "%", "!", "?", ":", "(",
    ];
    let b = s.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    'outer: while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c == b'n' {
            out.push(Tok::N);
            i += 1;
        } else if c.is_ascii_digit() {
            let start = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            out.push(Tok::Num(s[start..i].parse().ok()?));
        } else if c == b')' {
            out.push(Tok::Sym(")"));
            i += 1;
        } else {
            for sym in SYMS {
                if s[i..].starts_with(sym) {
                    out.push(Tok::Sym(sym));
                    i += sym.len();
                    continue 'outer;
                }
            }
            return None;
        }
    }
    Some(out)
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

fn parse_expr(s: &str) -> Option<Expr> {
    let mut p = Parser {
        toks: lex(s)?,
        pos: 0,
    };
    let e = p.ternary(0)?;
    (p.pos == p.toks.len()).then_some(e)
}

/// Maximum depth (safeguard against a malicious header).
const MAX_DEPTH: usize = 64;

impl Parser {
    fn peek_sym(&self) -> Option<&'static str> {
        match self.toks.get(self.pos) {
            Some(Tok::Sym(s)) => Some(s),
            _ => None,
        }
    }

    fn ternary(&mut self, depth: usize) -> Option<Expr> {
        if depth > MAX_DEPTH {
            return None;
        }
        let c = self.binary(0, depth)?;
        if self.peek_sym() == Some("?") {
            self.pos += 1;
            let a = self.ternary(depth + 1)?;
            if self.peek_sym() != Some(":") {
                return None;
            }
            self.pos += 1;
            let b = self.ternary(depth + 1)?;
            return Some(Expr::Cond(Box::new(c), Box::new(a), Box::new(b)));
        }
        Some(c)
    }

    fn binary(&mut self, min_prec: u8, depth: usize) -> Option<Expr> {
        let mut lhs = self.unary(depth)?;
        while let Some(sym) = self.peek_sym() {
            let (op, prec) = match sym {
                "||" => (Op::Or, 1),
                "&&" => (Op::And, 2),
                "==" => (Op::Eq, 3),
                "!=" => (Op::Ne, 3),
                "<" => (Op::Lt, 4),
                ">" => (Op::Gt, 4),
                "<=" => (Op::Le, 4),
                ">=" => (Op::Ge, 4),
                "+" => (Op::Add, 5),
                "-" => (Op::Sub, 5),
                "*" => (Op::Mul, 6),
                "/" => (Op::Div, 6),
                "%" => (Op::Rem, 6),
                _ => break,
            };
            if prec < min_prec {
                break;
            }
            self.pos += 1;
            let rhs = self.binary(prec + 1, depth + 1)?;
            lhs = Expr::Bin(op, Box::new(lhs), Box::new(rhs));
        }
        Some(lhs)
    }

    fn unary(&mut self, depth: usize) -> Option<Expr> {
        if depth > MAX_DEPTH {
            return None;
        }
        match self.toks.get(self.pos)?.clone() {
            Tok::N => {
                self.pos += 1;
                Some(Expr::N)
            }
            Tok::Num(v) => {
                self.pos += 1;
                Some(Expr::Num(v))
            }
            Tok::Sym("!") => {
                self.pos += 1;
                Some(Expr::Not(Box::new(self.unary(depth + 1)?)))
            }
            Tok::Sym("(") => {
                self.pos += 1;
                let e = self.ternary(depth + 1)?;
                if self.peek_sym() != Some(")") {
                    return None;
                }
                self.pos += 1;
                Some(e)
            }
            Tok::Sym(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idx(h: &str, n: u64) -> usize {
        PluralRule::parse(h).map(|r| r.index(n)).unwrap_or(99)
    }

    #[test]
    fn english_and_french() {
        let en = "nplurals=2; plural=(n != 1);";
        assert_eq!((idx(en, 0), idx(en, 1), idx(en, 2)), (1, 0, 1));
        let fr = "nplurals=2; plural=(n > 1);";
        assert_eq!((idx(fr, 0), idx(fr, 1), idx(fr, 2)), (0, 0, 1));
    }

    #[test]
    fn russian_and_polish() {
        let ru = "nplurals=3; plural=(n%10==1 && n%100!=11 ? 0 : n%10>=2 && n%10<=4 && (n%100<10 || n%100>=20) ? 1 : 2);";
        assert_eq!(
            (idx(ru, 1), idx(ru, 3), idx(ru, 5), idx(ru, 11), idx(ru, 21)),
            (0, 1, 2, 2, 0)
        );
        let pl = "nplurals=3; plural=(n==1 ? 0 : n%10>=2 && n%10<=4 && (n%100<10 || n%100>=20) ? 1 : 2);";
        assert_eq!((idx(pl, 1), idx(pl, 22), idx(pl, 25)), (0, 1, 2));
    }

    #[test]
    fn invalid_headers_are_rejected() {
        assert!(PluralRule::parse("nplurals=2; plural=(n >").is_none());
        assert!(PluralRule::parse("nplurals=0; plural=0;").is_none());
        assert!(PluralRule::parse("plural=n;").is_none());
        assert!(PluralRule::parse("nplurals=2; plural=n $ 2;").is_none());
        let deep = format!(
            "nplurals=2; plural={}n{};",
            "(".repeat(500),
            ")".repeat(500)
        );
        assert!(PluralRule::parse(&deep).is_none());
    }

    #[test]
    fn index_is_clamped_and_division_safe() {
        assert_eq!(idx("nplurals=2; plural=n;", 7), 1);
        assert_eq!(idx("nplurals=2; plural=1/0;", 7), 0);
        assert_eq!(idx("nplurals=1; plural=0;", u64::MAX), 0);
    }
}
