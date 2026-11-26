//! FANS-1/A CPDLC (HFDL seed `applications.md` §3): the ASN.1 module
//! (`data/cpdlc-fans1a.asn1`) read at run time, unaligned PER decoding of
//! it, and the trees of `data/cpdlc-json.csv` (libacars' JSON names).

use std::collections::HashMap;
use std::sync::OnceLock;

use serde_json::{Map, Value, json};

use super::Dir;

#[derive(Debug, Clone)]
enum Ty {
    Seq(Vec<Comp>),
    SeqOf(i64, i64, Box<Ty>),
    Choice(Vec<(String, Ty)>),
    Int(i64, i64),
    Enum(Vec<(String, i64)>),
    Ia5(i64, i64),
    Numeric(i64, i64),
    Bool,
    Null,
    Ref(String),
}

#[derive(Debug, Clone)]
struct Comp {
    name: String,
    ty: Ty,
    optional: bool,
}

struct Module {
    types: HashMap<String, Ty>,
    /// type -> (format, key, unit, scale)
    fmt: HashMap<String, (String, String, String, f64)>,
    labels: HashMap<String, String>,
}

// ── the module text ─────────────────────────────────────────────────────────

fn tokens(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in src.lines() {
        let line = match line.find("--") {
            Some(i) => &line[..i],
            None => line,
        };
        let mut cur = String::new();
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if c.is_alphanumeric() || c == '-' && !cur.is_empty() || c == '_' {
                cur.push(c);
            } else {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                if c == ':' && line[i..].starts_with("::=") {
                    out.push("::=".into());
                    i += 3;
                    continue;
                }
                if c == '.' && i + 1 < chars.len() && chars[i + 1] == '.' {
                    out.push("..".into());
                    i += 2;
                    continue;
                }
                if !c.is_whitespace() {
                    out.push(c.to_string());
                }
            }
            i += 1;
        }
        if !cur.is_empty() {
            out.push(cur);
        }
    }
    out
}

struct P {
    t: Vec<String>,
    i: usize,
}

impl P {
    fn peek(&self) -> &str {
        self.t.get(self.i).map(|s| s.as_str()).unwrap_or("")
    }
    fn next(&mut self) -> String {
        self.i += 1;
        self.t.get(self.i - 1).cloned().unwrap_or_default()
    }
    fn eat(&mut self, s: &str) -> bool {
        if self.peek() == s {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn num(&mut self) -> i64 {
        let neg = self.eat("-");
        let v: i64 = self.next().parse().unwrap_or(0);
        if neg { -v } else { v }
    }
    /// `( lb .. ub )` or `( n )`, maybe wrapped in SIZE.
    fn range(&mut self) -> (i64, i64) {
        self.eat("(");
        let size = self.eat("SIZE");
        if size {
            self.eat("(");
        }
        let lb = self.num();
        let ub = if self.eat("..") { self.num() } else { lb };
        self.eat(")");
        if size {
            self.eat(")");
        }
        (lb, ub)
    }
    fn skip_tag(&mut self) {
        if self.peek() == "[" {
            while self.next() != "]" {}
        }
    }
    fn ty(&mut self) -> Ty {
        self.skip_tag();
        match self.next().as_str() {
            "SEQUENCE" => {
                if self.peek() == "SIZE" || self.peek() == "(" {
                    let (lb, ub) = self.range();
                    self.eat("OF");
                    Ty::SeqOf(lb, ub, Box::new(self.ty()))
                } else if self.eat("OF") {
                    Ty::SeqOf(0, i64::MAX, Box::new(self.ty()))
                } else {
                    self.eat("{");
                    let mut comps = Vec::new();
                    while !self.eat("}") {
                        let name = self.next();
                        let ty = self.ty();
                        let optional = self.eat("OPTIONAL");
                        comps.push(Comp { name, ty, optional });
                        self.eat(",");
                    }
                    Ty::Seq(comps)
                }
            }
            "CHOICE" => {
                self.eat("{");
                let mut alts = Vec::new();
                while !self.eat("}") {
                    let name = self.next();
                    let ty = self.ty();
                    alts.push((name, ty));
                    self.eat(",");
                }
                Ty::Choice(alts)
            }
            "INTEGER" => {
                if self.peek() == "(" {
                    let (lb, ub) = self.range();
                    Ty::Int(lb, ub)
                } else {
                    Ty::Int(i64::MIN, i64::MAX)
                }
            }
            "ENUMERATED" => {
                self.eat("{");
                let mut items = Vec::new();
                let mut next = 0;
                while !self.eat("}") {
                    let name = self.next();
                    let v = if self.eat("(") {
                        let v = self.num();
                        self.eat(")");
                        v
                    } else {
                        next
                    };
                    next = v + 1;
                    items.push((name, v));
                    self.eat(",");
                }
                items.sort_by_key(|x| x.1);
                Ty::Enum(items)
            }
            "IA5String" => {
                let (lb, ub) = if self.peek() == "(" {
                    self.range()
                } else {
                    (0, i64::MAX)
                };
                Ty::Ia5(lb, ub)
            }
            "NumericString" => {
                let (lb, ub) = if self.peek() == "(" {
                    self.range()
                } else {
                    (0, i64::MAX)
                };
                Ty::Numeric(lb, ub)
            }
            "BOOLEAN" => Ty::Bool,
            "NULL" => Ty::Null,
            other => {
                // a constrained reference, e.g. `FANSX (SIZE ...)`, is not used; skip any constraint
                if self.peek() == "(" {
                    let _ = self.range();
                }
                Ty::Ref(other.to_string())
            }
        }
    }
}

fn module() -> &'static Module {
    static M: OnceLock<Module> = OnceLock::new();
    M.get_or_init(|| {
        let mut p = P {
            t: tokens(include_str!("../data/cpdlc-fans1a.asn1")),
            i: 0,
        };
        while p.i < p.t.len() && p.peek() != "BEGIN" {
            p.i += 1;
        }
        p.eat("BEGIN");
        let mut types = HashMap::new();
        while p.i < p.t.len() && p.peek() != "END" {
            let name = p.next();
            if !p.eat("::=") {
                continue;
            }
            let ty = p.ty();
            types.insert(name, ty);
        }
        let mut fmt = HashMap::new();
        for line in include_str!("../data/cpdlc-json.csv").lines().skip(1) {
            let f: Vec<&str> = line.split(',').collect();
            if f.len() >= 5 {
                fmt.insert(
                    f[0].to_string(),
                    (
                        f[1].to_string(),
                        f[2].to_string(),
                        f[3].to_string(),
                        f[4].parse().unwrap_or(1.0),
                    ),
                );
            }
        }
        let mut labels = HashMap::new();
        for rec in csv_rows(include_str!("../data/cpdlc-elements.csv"))
            .into_iter()
            .skip(1)
        {
            if rec.len() >= 4 {
                labels.insert(rec[2].clone(), rec[3].clone());
            }
        }
        Module { types, fmt, labels }
    })
}

/// Minimal CSV reading (quoted fields with commas).
pub fn csv_rows(s: &str) -> Vec<Vec<String>> {
    s.lines()
        .map(|l| {
            let mut out = Vec::new();
            let mut cur = String::new();
            let mut q = false;
            let mut chars = l.chars().peekable();
            while let Some(c) = chars.next() {
                match c {
                    '"' if q && chars.peek() == Some(&'"') => {
                        cur.push('"');
                        chars.next();
                    }
                    '"' => q = !q,
                    ',' if !q => out.push(std::mem::take(&mut cur)),
                    c => cur.push(c),
                }
            }
            out.push(cur);
            out
        })
        .collect()
}

// ── unaligned PER ───────────────────────────────────────────────────────────

struct Bits<'a> {
    b: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn read(&mut self, n: u32) -> Option<u64> {
        let mut v = 0u64;
        for _ in 0..n {
            let byte = *self.b.get(self.pos / 8)?;
            v = (v << 1) | ((byte >> (7 - self.pos % 8)) & 1) as u64;
            self.pos += 1;
        }
        Some(v)
    }
}

fn bits_for(range: i64) -> u32 {
    if range <= 1 {
        0
    } else {
        64 - ((range - 1) as u64).leading_zeros()
    }
}

/// A decoded value with the name of the type it was declared with.
#[derive(Debug, Clone)]
enum V {
    Seq(Vec<(String, Option<String>, V)>),
    SeqOf(Vec<(Option<String>, V)>),
    Choice(String, Option<String>, Box<V>),
    Int(i64),
    Enum(String),
    Str(String),
    Bool(bool),
    Null,
}

fn length(r: &mut Bits, lb: i64, ub: i64) -> Option<i64> {
    if ub == i64::MAX {
        // unconstrained length (not in this module): one octet form
        return Some(r.read(8)? as i64);
    }
    Some(lb + r.read(bits_for(ub - lb + 1))? as i64)
}

/// Decode `ty`; returns the value and the name of the named type (if any).
fn decode(ty: &Ty, r: &mut Bits, depth: u32) -> Option<(V, Option<String>)> {
    if depth > 64 {
        return None;
    }
    let m = module();
    Some(match ty {
        Ty::Ref(name) => {
            let inner = m.types.get(name)?;
            let (v, _) = decode(inner, r, depth + 1)?;
            (v, Some(name.clone()))
        }
        Ty::Seq(comps) => {
            let opts: Vec<bool> = comps
                .iter()
                .filter(|c| c.optional)
                .map(|_| r.read(1).map(|b| b == 1))
                .collect::<Option<_>>()?;
            let mut oi = 0;
            let mut out = Vec::new();
            for c in comps {
                if c.optional {
                    let present = opts[oi];
                    oi += 1;
                    if !present {
                        continue;
                    }
                }
                let (v, n) = decode(&c.ty, r, depth + 1)?;
                out.push((c.name.clone(), n, v));
            }
            (V::Seq(out), None)
        }
        Ty::SeqOf(lb, ub, el) => {
            let n = length(r, *lb, *ub)?;
            let mut out = Vec::new();
            for _ in 0..n {
                let (v, name) = decode(el, r, depth + 1)?;
                out.push((name, v));
            }
            (V::SeqOf(out), None)
        }
        Ty::Choice(alts) => {
            let i = r.read(bits_for(alts.len() as i64))? as usize;
            let (name, aty) = alts.get(i)?;
            let (v, n) = decode(aty, r, depth + 1)?;
            (V::Choice(name.clone(), n, Box::new(v)), None)
        }
        Ty::Int(lb, ub) => {
            if *lb == i64::MIN {
                return None;
            }
            (V::Int(lb + r.read(bits_for(ub - lb + 1))? as i64), None)
        }
        Ty::Enum(items) => {
            let i = r.read(bits_for(items.len() as i64))? as usize;
            (V::Enum(items.get(i)?.0.clone()), None)
        }
        Ty::Ia5(lb, ub) => {
            let n = length(r, *lb, *ub)?;
            let s: Option<String> = (0..n).map(|_| r.read(7).map(|c| c as u8 as char)).collect();
            (V::Str(s?), None)
        }
        Ty::Numeric(lb, ub) => {
            let n = length(r, *lb, *ub)?;
            const ALPHA: &[u8] = b" 0123456789";
            let s: Option<String> = (0..n)
                .map(|_| {
                    r.read(4)
                        .and_then(|c| ALPHA.get(c as usize).map(|&b| b as char))
                })
                .collect();
            (V::Str(s?), None)
        }
        Ty::Bool => (V::Bool(r.read(1)? == 1), None),
        Ty::Null => (V::Null, None),
    })
}

// ── the trees ──────────────────────────────────────────────────────────────

fn scalar_text(v: &V) -> String {
    match v {
        V::Int(i) => i.to_string(),
        V::Str(s) => s.clone(),
        V::Enum(e) => e.clone(),
        V::Bool(b) => if *b { "TRUE" } else { "FALSE" }.into(),
        _ => String::new(),
    }
}

fn find<'a>(v: &'a V, comp: &str) -> Option<&'a V> {
    match v {
        V::Seq(items) => items.iter().find(|(n, _, _)| n == comp).map(|(_, _, v)| v),
        _ => None,
    }
}

fn int(v: Option<&V>) -> i64 {
    match v {
        Some(V::Int(i)) => *i,
        _ => 0,
    }
}

/// Format `v` declared as `ty_name` into `out` (libacars `la_asn1_output`).
fn format(ty_name: Option<&str>, v: &V, out: &mut Map<String, Value>) {
    let m = module();
    let Some(name) = ty_name else { return };
    let Some((kind, key, unit, scale)) = m.fmt.get(name) else {
        return;
    };
    let val = match kind.as_str() {
        "omit" => return,
        "sequence" => {
            let mut o = Map::new();
            if let V::Seq(items) = v {
                for (_, n, cv) in items {
                    format(n.as_deref(), cv, &mut o);
                }
            }
            Value::Object(o)
        }
        "choice" | "element_choice" => {
            let mut o = Map::new();
            if kind == "element_choice"
                && let V::Choice(alt, _, _) = v
            {
                o.insert(
                    "choice_label".into(),
                    json!(m.labels.get(alt).cloned().unwrap_or_default()),
                );
            }
            if let V::Choice(alt, n, inner) = v {
                o.insert("choice".into(), json!(alt));
                let mut d = Map::new();
                format(n.as_deref(), inner, &mut d);
                o.insert("data".into(), Value::Object(d));
            }
            Value::Object(o)
        }
        "sequence_of" => {
            let mut arr = Vec::new();
            if let V::SeqOf(items) = v {
                for (n, iv) in items {
                    let mut o = Map::new();
                    format(n.as_deref(), iv, &mut o);
                    arr.push(Value::Object(o));
                }
            }
            Value::Array(arr)
        }
        "number" => json!({"val": int(Some(v)) as f64 * scale, "unit": unit}),
        "integer" => json!(int(Some(v))),
        "string" => json!(scalar_text(v)),
        "enum" => json!(scalar_text(v)),
        "bool" => json!(matches!(v, V::Bool(true))),
        "time" => json!({"hour": int(find(v, "hours")), "min": int(find(v, "minutes"))}),
        "timestamp" => {
            json!({"hour": int(find(v, "hours")), "min": int(find(v, "minutes")), "sec": int(find(v, "seconds"))})
        }
        "latitude" | "longitude" => {
            let (deg, dir) = if kind == "latitude" {
                ("latitudeDegrees", "latitudeDirection")
            } else {
                ("longitudeDegrees", "longitudeDirection")
            };
            let mut o = json!({"deg": int(find(v, deg))});
            if let Some(mm) = find(v, "minutesLatLon") {
                o["min"] = json!(int(Some(mm)) as f64 / 10.0);
            }
            o["dir"] = json!(find(v, dir).map(scalar_text).unwrap_or_default());
            o
        }
        "beacon_code" => {
            let s: String = match v {
                V::SeqOf(items) => items
                    .iter()
                    .map(|(_, d)| char::from(b'0' + int(Some(d)) as u8))
                    .collect(),
                _ => String::new(),
            };
            json!(s)
        }
        "label_only" => json!({}),
        _ => return,
    };
    out.insert(key.clone(), val);
}

/// A CPDLC message (`AT1`, `CR1`, `CC1`, `DR1` body) as libacars' tree.
pub fn parse(b: &[u8], dir: Dir) -> Value {
    if b.is_empty() {
        return json!({"err": false});
    }
    let top = if dir == Dir::Up {
        "FANSATCUplinkMessage"
    } else {
        "FANSATCDownlinkMessage"
    };
    let mut r = Bits { b, pos: 0 };
    let Some((v, _)) = decode(&Ty::Ref(top.to_string()), &mut r, 0) else {
        return json!({"err": true});
    };
    // every octet must be used (uper_decode_complete)
    if r.pos.div_ceil(8) < b.len() {
        return json!({"err": true});
    }
    let mut o = Map::new();
    o.insert("err".into(), json!(false));
    format(Some(top), &v, &mut o);
    Value::Object(o)
}

/// The element names of a decoded CPDLC tree (`dM56NULL`, …), in order.
pub fn elements(tree: &Value) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::Object(m) => {
                if let (Some(Value::String(c)), Some(_)) = (m.get("choice"), m.get("choice_label"))
                {
                    out.push(c.clone());
                }
                for x in m.values() {
                    walk(x, out);
                }
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    walk(tree, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_module_is_read_whole() {
        let m = module();
        assert!(m.types.len() > 200, "{}", m.types.len());
        assert!(m.types.contains_key("FANSATCUplinkMessage"));
        assert!(m.fmt.len() > 200);
        assert_eq!(
            m.labels.get("uM20Altitude").map(|s| s.as_str()),
            Some("CLIMB TO AND MAINTAIN [altitude]")
        );
    }

    #[test]
    fn climb_to_fl350_decodes() {
        // asn1tools: uM20Altitude altitudeFlightLevel 350, msg id 5
        let v = parse(&[0x02, 0x8a, 0x65, 0x00], Dir::Up);
        let s = v.to_string();
        assert!(s.contains("uM20Altitude"), "{s}");
        assert!(s.contains("350"), "{s}");
    }
}
