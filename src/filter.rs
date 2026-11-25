//! Matching names the way people type them: case and accents do not
//! matter, so "cafe" finds "Café" and "a" finds "Å". Shared by type to
//! filter and the search below a folder.

/// `s` in lower case with the accents of Latin letters taken off.
pub fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii() {
            out.push(c.to_ascii_lowercase());
            continue;
        }
        for l in c.to_lowercase() {
            match base(l) {
                Some(b) => out.push_str(b),
                None => out.push(l),
            }
        }
    }
    out
}

/// Whether `name` holds every word of `query`, which is already folded.
pub fn matches(name: &str, words: &[String]) -> bool {
    if words.is_empty() {
        return true;
    }
    let f = fold(name);
    words.iter().all(|w| f.contains(w.as_str()))
}

/// The words of a typed text, folded, for `matches`.
pub fn words(query: &str) -> Vec<String> {
    fold(query).split_whitespace().map(str::to_string).collect()
}

/// The plain letter(s) under an accented lower-case Latin letter.
fn base(c: char) -> Option<&'static str> {
    Some(match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
        'æ' => "ae",
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => "c",
        'ď' | 'đ' | 'ð' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => "e",
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => "g",
        'ĥ' | 'ħ' => "h",
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => "i",
        'ĵ' => "j",
        'ķ' => "k",
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => "l",
        'ñ' | 'ń' | 'ņ' | 'ň' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => "o",
        'œ' => "oe",
        'ŕ' | 'ŗ' | 'ř' => "r",
        'ś' | 'ŝ' | 'ş' | 'š' | 'ș' => "s",
        'ß' => "ss",
        'ţ' | 'ť' | 'ŧ' | 'ț' => "t",
        'þ' => "th",
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => "u",
        'ŵ' => "w",
        'ý' | 'ÿ' | 'ŷ' => "y",
        'ź' | 'ż' | 'ž' => "z",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_and_accents_do_not_matter() {
        assert_eq!(fold("Café Ölstuga ÅÄÖ"), "cafe olstuga aao");
        assert_eq!(fold("Straße"), "strasse");
        assert!(matches("Semester på Öland.jpg", &words("oland")));
        assert!(matches("Semester på Öland.jpg", &words("PA semester")));
        assert!(!matches("Semester på Öland.jpg", &words("gotland")));
        assert!(matches("anything", &words("   ")));
        // Letters outside Latin stay as they are, in lower case.
        assert_eq!(fold("ΑΒΓ日本"), "αβγ日本");
    }
}
