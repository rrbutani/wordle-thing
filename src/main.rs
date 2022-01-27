use std::{
    collections::HashSet,
    fmt::{self, Debug, Display},
    str::FromStr,
};

use chrono::{DateTime, Utc};
use color_eyre::{eyre::WrapErr, owo_colors::OwoColorize, Help, Result};
use egg_mode::{auth, tweet, KeyPair};
use futures::StreamExt;
use lazy_static::lazy_static;
use regex::Regex;
use reqwest::Url;
use soup::{NodeExt, QueryBuilderExt, Soup};
use structopt::StructOpt;
use tokio::runtime::Handle;

#[derive(Debug, StructOpt)]
struct Args {
    /// The root of the twitter thread to crawl.
    root_tweet_id: u64,

    /// Twitter API consumer key. Must be authorized to use the V2 API.
    ///
    /// If not specified this is grabbed from `$TWITTER_CONSUMER_KEY`.
    #[structopt(long, env = "TWITTER_CONSUMER_KEY")]
    consumer_key: String,

    /// Twitter API consumer secret. Must be authorized to use the V2 API.
    ///
    /// If not specified this is grabbed from `$TWITTER_CONSUMER_SECRET`.
    #[structopt(long, env = "TWITTER_CONSUMER_SECRET")]
    consumer_secret: String,

    /// Days to exclude.
    #[structopt(short, long, default_value = "0")]
    excludes: Vec<usize>,
}

const URL: &str = "https://www.powerlanguage.co.uk/wordle/";
const START_DATE: &str = "2021-06-19T00:00:00Z";

async fn get_valid_words_and_answers() -> (Vec<String>, Vec<String>) {
    let url = Url::parse(URL).unwrap();
    let main_page = reqwest::get(url.clone())
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    let script = Soup::new(&main_page)
        .tag("script")
        .find_all()
        .filter_map(|script| script.get("src"))
        .filter(|src| src.starts_with("main"))
        .last()
        .expect("main script on the wordle page");

    let script_url = url.join(&script).unwrap();
    let script = reqwest::get(script_url)
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    const DAY1_ANS: &str = "cigar";
    let answers_starting_idx = script.find(&format!("[\"{DAY1_ANS}\",")).unwrap();
    let data = &script[answers_starting_idx..];
    let answers_ending_idx = data.find(']').unwrap();
    let answers = &data[1..answers_ending_idx];

    let word_list_starting_idx = (&data[1..]).find('[').unwrap();
    let word_list = &data[1 + word_list_starting_idx..];
    let word_list_ending_idx = word_list.find(']').unwrap();
    let word_list = &word_list[1..word_list_ending_idx];

    let parse = |s: &str| {
        debug_assert!(s.len() == 7 && &s[0..1] == "\"" && &s[6..7] == "\"");
        s[1..=5].to_string()
    };

    (
        word_list.split(',').map(parse).collect::<Vec<_>>(),
        answers.split(',').map(parse).collect::<Vec<_>>(),
    )
}

struct WordleData {
    valid_words: Vec<String>,
    answers: Vec<String>,
}

lazy_static! {
    static ref WORDLE_DATA: WordleData = tokio::task::block_in_place(|| {
        let (valid_words, answers) =
            Handle::current().block_on(async move { get_valid_words_and_answers().await });
        WordleData {
            valid_words,
            answers,
        }
    });
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
enum Cell {
    Partial,
    Match,
    Nop,
}

impl Display for Cell {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            fmt,
            "{}",
            match self {
                Cell::Nop => '⬛',
                Cell::Partial => '🟨',
                Cell::Match => '🟩',
            }
        )
    }
}

impl TryFrom<char> for Cell {
    type Error = ();

    fn try_from(c: char) -> Result<Self, Self::Error> {
        Ok(match c {
            '⬛' | '⬜' => Cell::Nop,
            '🟨' => Cell::Partial,
            '🟩' => Cell::Match,
            _ => return Err(()),
        })
    }
}

struct GuessDisplayWrapper<'a>(&'a Guess);
impl Display for GuessDisplayWrapper<'_> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        for i in self.0 {
            write!(fmt, "{}", i)?
        }

        Ok(())
    }
}

trait GuessDisplay {
    fn display(&self) -> GuessDisplayWrapper;
}
impl GuessDisplay for Guess {
    fn display(&self) -> GuessDisplayWrapper {
        GuessDisplayWrapper(self)
    }
}

type Guess = [Cell; 5];

#[derive(Clone)]
enum Constraint {
    IsOneOf(HashSet<char>),
    IsNoneOf(HashSet<char>),
}

impl Debug for Constraint {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        let to_list = |h: &HashSet<_>| {
            let mut v = h.iter().copied().collect::<Vec<_>>();
            v.sort();
            v.iter().collect::<String>()
        };
        match self {
            Constraint::IsOneOf(h) => write!(fmt, "+[{}]", to_list(h)),
            Constraint::IsNoneOf(h) => write!(fmt, "-[{}]", to_list(h)),
        }
    }
}

fn solve(guesses: &[(Guess, &str)]) -> Option<Vec<&'static str>> {
    const V: Vec<Constraint> = vec![];
    let mut constraints = [V; 5];
    for (guess, word) in guesses {
        let chars: [char; 5] = word.chars().collect::<Vec<_>>().try_into().unwrap();
        for (i, &c) in chars.iter().enumerate() {
            let cell = guess[i];
            let constraint = match cell {
                Cell::Match => Constraint::IsOneOf(HashSet::from([c])),
                Cell::Partial => {
                    // This means this char can be one of the chars in `word`
                    // that *wasn't* guessed right, except for the char that's
                    // actually expected at this spot.
                    Constraint::IsOneOf(
                        guess
                            .iter()
                            .enumerate()
                            .filter(|(idx, &c)| !matches!(c, Cell::Match) && *idx != i)
                            .map(|(i, _)| chars[i])
                            .collect(),
                    )
                }
                Cell::Nop => {
                    // This means this char isn't any of the chars in `word`
                    // that *weren't* guessed right, else we'd have gotten a
                    // partial.
                    //
                    // Note that this is *not* equivalent to saying that this
                    // char isn't any of the chars in `word`; if a char
                    // occurs once and is guessed for that spot correctly,
                    // other "guesses" of that char in the word will not
                    // yield partials.
                    //
                    // For example, a guess of "fluff" will yield `🟩⬛⬛⬛⬛`
                    // when the word is "foggy". The trailing two `f`s do not
                    // yield partials.
                    Constraint::IsNoneOf(
                        guess
                            .iter()
                            .enumerate()
                            .filter(|(_, &c)| !matches!(c, Cell::Match))
                            .map(|(i, _)| chars[i])
                            .collect(),
                    )
                }
            };
            constraints[i].push(constraint);
        }
    }

    // dbg!(&constraints);

    // Next, solve for each letter:
    let mut impossible = false;
    let regex = constraints
        .iter()
        .map(|constraints| {
            constraints.iter().fold(
                ('a'..='z').collect::<HashSet<_>>(),
                |state_space, c| match c {
                    Constraint::IsOneOf(h) => state_space.intersection(h).copied().collect(),
                    Constraint::IsNoneOf(h) => state_space.difference(h).copied().collect(),
                },
            )
        })
        .enumerate()
        .map(|(idx, allowed)| {
            if allowed.len() == 0 {
                println!(":-( no possible values for letter {}", idx + 1);
                impossible = true;
            }
            let mut allowed = allowed.into_iter().collect::<Vec<_>>();
            allowed.sort();
            format!("[{}]", allowed.iter().collect::<String>())
        })
        .collect::<String>();

    if impossible {
        return None;
    }

    println!("Using regex: `{regex}`.");

    let re = Regex::new(&format!("^{}$", regex)).unwrap();
    let possible_guesses: Vec<_> = WORDLE_DATA
        .valid_words
        .iter()
        .chain(WORDLE_DATA.answers.iter())
        .map(|w| &**w)
        .filter(|w| re.is_match(w))
        .collect();
    if possible_guesses.len() == 0 {
        return None;
    }

    Some(possible_guesses)
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    let args = Args::from_args();
    let excludes = args.excludes.iter().collect::<HashSet<_>>();

    let token = KeyPair::new(args.consumer_key, args.consumer_secret);
    let token = auth::bearer_token(&token)
        .await
        .wrap_err("Unable to authenticate!")
        .suggestion("check your consumer key/consumer secret?")?;

    let root = tweet::show(args.root_tweet_id, &token)
        .await
        .wrap_err_with(|| {
            format!(
                "Failed to find the specified root tweet (`{}`)",
                args.root_tweet_id
            )
        })?;

    if Utc::now().signed_duration_since(root.created_at).num_days() >= 7 {
        eprintln!(
            "{}: The given root tweet is {}!\n\n\
            The Twitter Recent Search API will not find tweets that are over \
            seven days old.\n\
            The Full-archive Search API will but that API is currently limited \
            to Academic Research users only.\n\n\
            See this page for more details: {}.\n",
            "WARNING".yellow().bold(),
            "over 7 days old".bold().italic(),
            "https://developer.twitter.com/en/docs/twitter-api/tweets/search/introduction"
                .underline()
                .italic(),
        );
    }

    let day_one = DateTime::<Utc>::from_str(START_DATE).unwrap();

    let root_user_id = root.user.as_ref().unwrap().id;
    let mut children = tweet::all_children_raw(args.root_tweet_id, &token).await;
    let mut guesses = Vec::with_capacity(7);
    while let Some(t) = children.next().await {
        let t = t?;
        let author_id = t.author_id.unwrap();

        if author_id != root_user_id {
            continue;
        }

        let text = &t.text;
        let first_guess = if let Some(first_guess) = text
            .lines()
            .filter_map(|l| {
                if l.chars().count() != 5 {
                    return None;
                }

                let mut res = [Cell::Nop; 5];
                for (idx, c) in l.chars().enumerate() {
                    res[idx] = if let Ok(c) = c.try_into() {
                        c
                    } else {
                        return None;
                    };
                }

                Some(res)
            })
            .next()
        {
            first_guess
        } else {
            continue;
        };

        // If the tweet starts with "Wordle <day number>" we'll use that day number.
        let day: usize = if let Some(day) = text
            .lines()
            .next()
            .and_then(|l| l.strip_prefix("Wordle "))
            .and_then(|l| l.split_whitespace().next())
            .and_then(|l| l.trim().parse().ok())
        {
            day
        } else {
            // Otherwise we'll guess from the tweet date.
            let author_date = t.created_at.unwrap();
            author_date
                .signed_duration_since(day_one)
                .num_days()
                .try_into()
                .unwrap()
        };
        if excludes.contains(&day) {
            continue;
        }

        let word = &*WORDLE_DATA.answers[day];
        println!("[{day:3}] {} ({word})", first_guess.display());

        guesses.push((first_guess, word));
    }

    if let Some(potential_answers) = solve(&*guesses) {
        println!();
        if potential_answers.len() == 1 {
            println!("Is your first guess.. {}?", potential_answers[0].bold());
        } else if potential_answers.len() <= 12 {
            println!("Couldn't exactly figure out your preferred first guess but we have some guesses: {:#?}", potential_answers);
        } else {
            println!(
                "Couldn't figure it out! (we found {} possibilities, too many)",
                potential_answers.len()
            );
        }
    } else {
        std::process::exit(2);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    //! https://www.devangthakkar.com/wordle_archive/?221 is useful for
    //! making test cases.

    use super::*;

    macro_rules! test {
        (
            $nom:ident,
            days: {
                $($day:literal: $g:literal),* $(,)?
            },
            expected: $ex:expr $(,)?
        ) => {
            #[tokio::test(flavor = "multi_thread")]
            async fn $nom() {
                let guesses = [$(
                    (
                        TryInto::<[_; 5]>::try_into($g.chars()
                            .map(|c| TryInto::<Cell>::try_into(c).unwrap())
                            .collect::<Vec<_>>()).unwrap(),
                            &*WORDLE_DATA.answers[$day],
                    ),
                )*];

                let got = solve(&guesses);
                let expected = $ex;
                let expected = match expected {
                    Some(ref x) => Some(x.as_slice()),
                    None => None,
                };
                if let None = expected {
                    assert_eq!(got, None);
                } else {
                    assert_eq!(got.as_deref(), expected);
                }
            }
        };
    }

    test! {
        alive,
        days: {
            221: "🟨⬛⬛⬛⬛",
            220: "🟨⬛⬛⬛⬛",
            219: "⬛🟨⬛⬛⬛",
            218: "⬛⬛🟩⬛⬛",
            217: "⬛⬛🟨⬛🟩",
            216: "⬛⬛🟩⬛⬛",
            215: "⬛⬛⬛⬛⬛",
        },
        expected: Some(["alive"]),
    }
}
