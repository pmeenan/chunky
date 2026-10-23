//! Deterministic synthetic inputs for tests and the native-vs-wasm
//! identity check. Small and generated, never corpus data (D-007).

/// A 64-bit LCG; returns the high bits of each step.
pub fn lcg(seed: u64) -> impl FnMut() -> usize {
    let mut x = seed;
    move || {
        x = x
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (x >> 33) as usize
    }
}

/// Templated HTML list rows with pseudo-random words and ids.
pub fn templated(len: usize) -> Vec<u8> {
    const WORDS: [&str; 12] = [
        "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india",
        "juliet", "kilo", "lima",
    ];
    let mut next = lcg(7);
    let mut v = Vec::with_capacity(len + 256);
    while v.len() < len {
        let (a, b, id) = (next() % 12, next() % 12, next() % 100_000);
        v.extend_from_slice(
            format!(
                "<li class=\"item {}\"><a href=\"/{}/{id}\">{} {}</a><span>{id}</span></li>\n",
                WORDS[a], WORDS[b], WORDS[b], WORDS[a]
            )
            .as_bytes(),
        );
    }
    v.truncate(len);
    v
}

/// English-like prose, so q5+ emits static-dictionary references with
/// transforms (capitalization, suffixes).
pub fn prose(len: usize) -> Vec<u8> {
    const WORDS: [&str; 24] = [
        "the",
        "information",
        "available",
        "government",
        "between",
        "development",
        "national",
        "community",
        "following",
        "because",
        "different",
        "important",
        "something",
        "experience",
        "however",
        "everything",
        "university",
        "according",
        "together",
        "political",
        "although",
        "children",
        "possible",
        "question",
    ];
    let mut next = lcg(11);
    let mut v = Vec::with_capacity(len + 32);
    while v.len() < len {
        let w = WORDS[next() % WORDS.len()];
        match next() % 7 {
            0 => {
                let mut c = w.as_bytes().to_vec();
                c[0] = c[0].to_ascii_uppercase();
                v.extend_from_slice(&c);
            }
            1 => v.extend_from_slice(format!("{w}. ").as_bytes()),
            2 => v.extend_from_slice(format!("{w}, ").as_bytes()),
            3 => v.extend_from_slice(format!("\"{w}\" ").as_bytes()),
            _ => v.extend_from_slice(w.as_bytes()),
        }
        v.push(b' ');
    }
    v.truncate(len);
    v
}

/// Byte runs and short periods (overlapping copies at distance 1..5).
pub fn runs(len: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(len);
    let mut next = lcg(3);
    while v.len() < len {
        let b = b"a<>/ \n0"[next() % 7];
        let n = 1 + next() % 300;
        if next().is_multiple_of(3) {
            // A short period: overlapping copies at distance 2..5.
            let p = 2 + next() % 4;
            let pat: Vec<u8> = (0..p).map(|i| b'a' + i as u8).collect();
            v.extend(pat.iter().cycle().take(n));
        } else {
            v.extend(std::iter::repeat_n(b, n));
        }
    }
    v.truncate(len);
    v
}

/// Incompressible bytes.
pub fn random(len: usize) -> Vec<u8> {
    let mut next = lcg(5);
    (0..len).map(|_| next() as u8).collect()
}

/// Text, binary and repeats interleaved, to vary literal contexts.
pub fn mixed(len: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(len);
    let (t, r, p) = (templated(len), random(len), prose(len));
    let mut i = 0;
    while v.len() < len {
        v.extend_from_slice(&t[i..(i + 700).min(len)]);
        v.extend_from_slice(&r[i..(i + 90).min(len)]);
        v.extend_from_slice(&p[i..(i + 400).min(len)]);
        i = (i + 700) % (len / 2);
    }
    v.truncate(len);
    v
}

/// The standard fixture set, by name.
pub fn fixtures() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("templated", templated(60_000)),
        ("prose", prose(40_000)),
        ("runs", runs(20_000)),
        ("random", random(9_000)),
        ("mixed", mixed(50_000)),
        ("tiny", b"<!doctype html>".to_vec()),
        ("one", b"x".to_vec()),
    ]
}
