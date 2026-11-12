//! English cardinal and ordinal numbers, a port of the `num2words` package's
//! `lang_EN` (checked against it by `tests/data/num2words.json`).

#[derive(Clone, Debug)]
enum Node {
    Pair(String, u128),
    List(Vec<Node>),
}

const HIGH: [(u32, &str); 12] = [
    (36, "undecillion"),
    (33, "decillion"),
    (30, "nonillion"),
    (27, "octillion"),
    (24, "septillion"),
    (21, "sextillion"),
    (18, "quintillion"),
    (15, "quadrillion"),
    (12, "trillion"),
    (9, "billion"),
    (6, "million"),
    (3, "thousand"),
];

const MID: [(u128, &str); 12] = [
    (100, "hundred"),
    (90, "ninety"),
    (80, "eighty"),
    (70, "seventy"),
    (60, "sixty"),
    (50, "fifty"),
    (40, "forty"),
    (30, "thirty"),
    (20, "twenty"),
    (19, "nineteen"),
    (18, "eighteen"),
    (17, "seventeen"),
];

const LOW: [&str; 17] = [
    "sixteen", "fifteen", "fourteen", "thirteen", "twelve", "eleven", "ten", "nine", "eight",
    "seven", "six", "five", "four", "three", "two", "one", "zero",
];

/// Every card, largest first (the order `splitnum` walks them in).
fn cards() -> Vec<(u128, String)> {
    let mut v: Vec<(u128, String)> = Vec::new();
    for (exp, name) in HIGH {
        v.push((10u128.pow(exp), name.to_string()));
    }
    for (n, w) in MID {
        v.push((n, w.to_string()));
    }
    for (i, w) in LOW.iter().enumerate() {
        v.push((16 - i as u128, w.to_string()));
    }
    v
}

fn merge(l: (String, u128), r: (String, u128)) -> (String, u128) {
    let (lt, ln) = l;
    let (rt, rn) = r;
    if ln == 1 && rn < 100 {
        (rt, rn)
    } else if 100 > ln && ln > rn {
        (format!("{lt}-{rt}"), ln + rn)
    } else if ln >= 100 && 100 > rn {
        (format!("{lt} and {rt}"), ln + rn)
    } else if rn > ln {
        (format!("{lt} {rt}"), ln * rn)
    } else {
        (format!("{lt}, {rt}"), ln + rn)
    }
}

fn splitnum(value: u128, cards: &[(u128, String)]) -> Vec<Node> {
    for (elem, word) in cards {
        if *elem > value {
            continue;
        }
        let mut out = Vec::new();
        let (div, modv) = if value == 0 {
            (1, 0)
        } else {
            (value / elem, value % elem)
        };
        if div == 1 {
            out.push(Node::Pair("one".into(), 1));
        } else {
            out.push(Node::List(splitnum(div, cards)));
        }
        out.push(Node::Pair(word.clone(), *elem));
        if modv > 0 {
            out.push(Node::List(splitnum(modv, cards)));
        }
        return out;
    }
    Vec::new()
}

fn clean(mut val: Vec<Node>) -> Node {
    loop {
        if val.len() == 1 {
            return val.remove(0);
        }
        let mut out: Vec<Node> = Vec::new();
        if let (Node::Pair(..), Node::Pair(..)) = (&val[0], &val[1]) {
            let rest: Vec<Node> = val.split_off(2);
            let right = val.pop().unwrap();
            let left = val.pop().unwrap();
            if let (Node::Pair(lt, ln), Node::Pair(rt, rn)) = (left, right) {
                let (t, n) = merge((lt, ln), (rt, rn));
                out.push(Node::Pair(t, n));
            }
            if !rest.is_empty() {
                out.push(Node::List(rest));
            }
        } else {
            for elem in val {
                match elem {
                    Node::List(mut l) => {
                        if l.len() == 1 {
                            out.push(l.remove(0));
                        } else {
                            out.push(clean(l));
                        }
                    }
                    p => out.push(p),
                }
            }
        }
        val = out;
    }
}

/// "one hundred and one", "twelve thousand, three hundred and forty-five".
pub fn cardinal(n: u128) -> String {
    let c = cards();
    match clean(splitnum(n, &c)) {
        Node::Pair(t, _) => t,
        Node::List(_) => String::new(),
    }
}

fn ordinal_word(word: &str) -> String {
    let w = word.to_lowercase();
    let fixed = match w.as_str() {
        "one" => Some("first"),
        "two" => Some("second"),
        "three" => Some("third"),
        "four" => Some("fourth"),
        "five" => Some("fifth"),
        "six" => Some("sixth"),
        "seven" => Some("seventh"),
        "eight" => Some("eighth"),
        "nine" => Some("ninth"),
        "ten" => Some("tenth"),
        "eleven" => Some("eleventh"),
        "twelve" => Some("twelfth"),
        _ => None,
    };
    if let Some(f) = fixed {
        return f.to_string();
    }
    let mut w = w;
    if w.ends_with('y') {
        w.pop();
        w.push_str("ie");
    }
    w.push_str("th");
    w
}

/// "first", "twenty-first", "one hundredth".
pub fn ordinal(n: u128) -> String {
    let card = cardinal(n);
    let mut words: Vec<&str> = card.split(' ').collect();
    let last = words.pop().unwrap_or("");
    let mut dashed: Vec<String> = last.split('-').map(str::to_string).collect();
    let tail = dashed.pop().unwrap_or_default();
    dashed.push(ordinal_word(&tail));
    let joined = dashed.join("-");
    words.push(&joined);
    words.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_python_num2words() {
        let raw = include_str!("../tests/data/num2words.json");
        let rows: Vec<(u64, String, String)> = serde_json::from_str(raw).unwrap();
        assert!(rows.len() > 1000);
        for (n, card, ord) in rows {
            let n = n as u128;
            assert_eq!(cardinal(n), card, "cardinal {n}");
            assert_eq!(ordinal(n), ord, "ordinal {n}");
        }
    }
}
