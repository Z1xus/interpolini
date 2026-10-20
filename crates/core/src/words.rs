use crate::config::Config;

// bip 39, each word is unique in its first four letters
const LIST: &str = include_str!("../words/english.txt");
// a change of the fields, the defaults, the choices or the common values needs a new version
const VERSION: u64 = 1;
// the checksum takes the rest of the last word, at least this many bits
const CHECK: usize = 6;
// a raw value of only these letters takes 4 bits a letter
const DIGITS: &str = "0123456789.-,[]x";

// values that take a few bits, other values go in raw
fn common(section: &str, key: &str) -> &'static [&'static str] {
    match (section, key) {
        ("dedup", "repeats") => &["100", "75", "50", "25"],
        ("pre-interpolation", "fps") => &["2x", "3x", "4x", "120", "240"],
        ("interpolation", "fps") => &[
            "960", "120", "240", "360", "480", "600", "720", "1200", "1440", "1920", "2400",
            "2880", "3840", "2x", "4x", "8x",
        ],
        ("rife", "model") => &[
            "rife-v4.6",
            "rife-v4.15-lite",
            "rife-v4.22-lite",
            "rife-v4.25",
            "rife-v4.26",
        ],
        ("mask", "peel limit") => &["6", "2", "4", "8", "10", "12", "16"],
        ("mask", "limit") => &["4.5", "2", "3", "4", "5", "6", "8"],
        ("mask", "edge") => &["5", "2", "3", "4", "6", "8", "10"],
        ("mask", "tolerance") => &["30", "10", "20", "25", "35", "40", "50"],
        ("blending", "fps") => &["60", "24", "30", "50", "120", "144", "165", "240"],
        ("blending", "intensity") => &["1", "0.5", "0.75", "1.25", "1.5", "2", "3"],
        ("blending", "gamma") => &["1", "1.5", "2", "2.2", "2.4", "3"],
        ("blending", "weighting") => &[
            "equal",
            "gaussian",
            "gaussian_sym",
            "pyramid",
            "ascending",
            "descending",
            "vegas",
        ],
        ("color", "brightness" | "contrast" | "saturation") => {
            &["1", "0.8", "0.9", "1.1", "1.2", "1.3", "1.5"]
        }
        ("color", "hue") => &["0", "-10", "-5", "5", "10"],
        ("output", "quality") => &[
            "20", "10", "12", "14", "15", "16", "17", "18", "19", "21", "22", "23", "24", "25",
            "26", "28",
        ],
        ("output", "size") => &["10", "8", "15", "20", "25", "50", "100", "250", "500"],
        _ => &[],
    }
}

struct Field {
    section: &'static str,
    key: &'static str,
    value: String,
    choices: &'static [&'static str],
}

fn fields(config: &Config) -> Vec<Field> {
    let mut config = config.clone();
    let fields = config.fields().into_iter();
    fields
        .map(|(section, key, value)| Field {
            section,
            key,
            value: value.write(),
            choices: value.choices(),
        })
        .collect()
}

impl Field {
    // the values a changed field can take, the default is not one
    fn options(&self) -> Vec<&'static str> {
        let all = match self.choices {
            [] => common(self.section, self.key),
            choices => choices,
        };
        all.iter()
            .copied()
            .filter(|option| *option != self.value)
            .collect()
    }
}

fn width(count: usize) -> usize {
    (usize::BITS - count.saturating_sub(1).leading_zeros()) as usize
}

fn hash(bits: &[bool]) -> u64 {
    let start = 0xcbf2_9ce4_8422_2325_u64;
    bits.iter().fold(start, |hash, bit| {
        (hash ^ *bit as u64).wrapping_mul(0x100_0000_01b3)
    })
}

#[derive(Default)]
struct Writer(Vec<bool>);

impl Writer {
    fn put(&mut self, value: u64, count: usize) {
        self.0
            .extend((0..count).rev().map(|at| value >> at & 1 == 1));
    }

    // elias gamma, small numbers take few bits
    fn gamma(&mut self, number: u64) {
        let count = width(number as usize + 1);
        self.put(0, count - 1);
        self.put(number, count);
    }
}

struct Reader<'a> {
    bits: &'a [bool],
    at: usize,
    // the words ran out, so the code can still be the start of a code
    short: bool,
}

impl Reader<'_> {
    fn take(&mut self, count: usize) -> Option<u64> {
        let Some(bits) = self.bits.get(self.at..self.at + count) else {
            self.short = true;
            return None;
        };
        self.at += count;
        Some(bits.iter().fold(0, |value, bit| value << 1 | *bit as u64))
    }

    fn gamma(&mut self) -> Option<u64> {
        let zeros = self.bits[self.at..].iter().take_while(|bit| !**bit).count();
        if zeros > 32 {
            return None;
        }
        self.at += zeros;
        self.take(zeros + 1)
    }
}

// a lut is a file on one computer, so a code leaves it out
pub fn encode(config: &Config) -> Vec<&'static str> {
    let mut config = config.clone();
    config.color.lut.clear();
    let base = fields(&Config::default());
    let mut out = Writer::default();
    out.put(VERSION, 3);
    let changed: Vec<(usize, String)> = fields(&config)
        .into_iter()
        .enumerate()
        .filter(|(index, field)| field.value != base[*index].value)
        .map(|(index, field)| (index, field.value))
        .collect();
    out.gamma(changed.len() as u64 + 1);
    let mut next = 0;
    for (index, value) in changed {
        out.gamma((index - next + 1) as u64);
        next = index + 1;
        let field = &base[index];
        let options = field.options();
        let found = options.iter().position(|option| *option == value);
        if field.choices.is_empty() {
            out.put(found.is_none() as u64, 1);
        }
        match found {
            Some(found) => out.put(found as u64, width(options.len())),
            None => {
                let tight: String = match field.key {
                    "weighting" => value.chars().filter(|letter| *letter != ' ').collect(),
                    _ => value.clone(),
                };
                if tight.chars().all(|letter| DIGITS.contains(letter)) {
                    out.put(0, 1);
                    out.gamma(tight.len() as u64 + 1);
                    for letter in tight.chars() {
                        out.put(DIGITS.find(letter).unwrap_or(0) as u64, 4);
                    }
                } else {
                    out.put(1, 1);
                    out.gamma(value.len() as u64 + 1);
                    for byte in value.bytes() {
                        out.put(byte.into(), 8);
                    }
                }
            }
        }
    }
    let pad = (11 - (out.0.len() + CHECK) % 11) % 11 + CHECK;
    out.put(hash(&out.0), pad);
    let list: Vec<&str> = LIST.split_whitespace().collect();
    let words = out.0.chunks(11).map(|word| {
        let index = word.iter().fold(0, |index, bit| index << 1 | *bit as usize);
        list[index]
    });
    words.collect()
}

pub enum Wrong {
    // the last word is the start of a code word
    Typing,
    Word(String),
    // more words can still make a code
    Short,
    Code,
}

pub fn decode(text: &str) -> Result<Config, Wrong> {
    let list: Vec<&str> = LIST.split_whitespace().collect();
    let mut bits = Writer::default();
    let typed: Vec<String> = text
        .split(|letter: char| !letter.is_alphabetic())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect();
    for (at, word) in typed.iter().enumerate() {
        let known = |known: &&str| {
            let start = (known.get(..4), word.get(..4));
            known == word || matches!(start, (Some(known), Some(word)) if known == word)
        };
        let Some(index) = list.iter().position(known) else {
            let last = at + 1 == typed.len() && text.ends_with(char::is_alphabetic);
            let start = list.iter().any(|known| known.starts_with(word.as_str()));
            return Err(if last && start {
                Wrong::Typing
            } else {
                Wrong::Word(word.clone())
            });
        };
        bits.put(index as u64, 11);
    }
    let bits = &bits.0;
    let mut input = Reader {
        bits,
        at: 0,
        short: false,
    };
    let Some(config) = read(&mut input) else {
        return Err(if input.short {
            Wrong::Short
        } else {
            Wrong::Code
        });
    };
    let data = input.at;
    let pad = bits.len() - data;
    if pad < CHECK {
        return Err(Wrong::Short);
    }
    let check = hash(&bits[..data]) & ((1 << pad.min(63)) - 1);
    if pad < CHECK + 11 && input.take(pad) == Some(check) {
        Ok(config)
    } else {
        Err(Wrong::Code)
    }
}

fn read(input: &mut Reader) -> Option<Config> {
    let base = fields(&Config::default());
    let mut config = Config::default();
    let mut fields = config.fields();
    if input.take(3)? != VERSION {
        return None;
    }
    let mut next = 0;
    for _ in 1..input.gamma()? {
        let index = next + input.gamma()? as usize - 1;
        next = index + 1;
        let field = base.get(index)?;
        let options = field.options();
        let raw = field.choices.is_empty() && input.take(1)? == 1;
        let value = if raw {
            let digits = input.take(1)? == 0;
            let length = input.gamma()? - 1;
            let mut bytes = Vec::new();
            for _ in 0..length {
                bytes.push(if digits {
                    DIGITS.as_bytes()[input.take(4)? as usize]
                } else {
                    input.take(8)? as u8
                });
            }
            String::from_utf8(bytes).ok()?
        } else {
            let found = input.take(width(options.len()))?;
            options.get(found as usize)?.to_string()
        };
        fields[index].2.read(&value);
    }
    drop(fields);
    Some(config)
}

pub fn changes(from: &Config, to: &Config) -> Vec<(&'static str, &'static str, String)> {
    let before = fields(from);
    let after = fields(to).into_iter().zip(before);
    let changed = after.filter(|(field, before)| field.value != before.value);
    changed
        .map(|(field, _)| (field.section, field.key, field.value))
        .collect()
}
