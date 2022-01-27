use std::{str::FromStr, fmt::{self, Display}};

use color_eyre::{eyre::WrapErr, Help, owo_colors::OwoColorize, Result};
use chrono::{Utc, DateTime};
use egg_mode::{auth, tweet, KeyPair};
use futures::StreamExt;
use reqwest::Url;
use soup::{Soup, QueryBuilderExt, NodeExt};
use structopt::StructOpt;

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
    let word_list = &data[1+word_list_starting_idx..];
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

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
enum Cell {
    Partial,
    Match,
    Nop,
}

impl Display for Cell {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(fmt, "{}", match self {
            Cell::Nop => '⬛',
            Cell::Partial => '🟨',
            Cell::Match => '🟩',
        })
    }
}

struct GuessDisplayWrapper<'a>(&'a [Cell; 5]);
impl Display for GuessDisplayWrapper<'_> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        for i in self.0 {
            write!(fmt, "{}", i)?
        }

        Ok(())
    }
}

trait GuessDisplay { fn display(&self) -> GuessDisplayWrapper; }
impl GuessDisplay for [Cell; 5] {
    fn display(&self) -> GuessDisplayWrapper { GuessDisplayWrapper(self) }
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    // let args = Args::from_args();

    let (words, answers) = get_valid_words_and_answers().await;
    // dbg!(answers);

    let args = Args::from_args();

    let token = KeyPair::new(
        args.consumer_key,
        args.consumer_secret,
    );
    let token = auth::bearer_token(&token)
        .await
        .wrap_err("Unable to authenticate!")
        .suggestion("check your consumer key/consumer secret?")?;

    let root = tweet::show(args.root_tweet_id, &token)
        .await
        .wrap_err_with(|| format!("Failed to find the specified root tweet (`{}`)", args.root_tweet_id))?;

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
            "https://developer.twitter.com/en/docs/twitter-api/tweets/search/introduction".underline().italic(),
        );
    }

    let day_one = DateTime::<Utc>::from_str(START_DATE).unwrap();

    let root_user_id = root.user.as_ref().unwrap().id;
    let mut children = tweet::all_children_raw(args.root_tweet_id, &token).await;
    while let Some(t) = children.next().await {
        let t = t?;
        let author_id = t.author_id.unwrap();

        if author_id != root_user_id {
            continue;
        }

        let text = &t.text;
        let first_guess = if let Some(first_guess) = text.lines().filter_map(|l| {
            if l.chars().count() != 5 {
                return None
            }

            let mut res = [Cell::Nop; 5];
            for (idx, c) in l.chars().enumerate() {
                match c {
                    '⬛' => res[idx] = Cell::Nop,
                    '🟨' => res[idx] = Cell::Partial,
                    '🟩' => res[idx] = Cell::Match,
                    _ => return None,
                }
            }

            Some(res)
        }).next() {
            first_guess
        } else {
            continue;
        };


        let author_date = t.created_at.unwrap();
        // dbg!(t);
        let day: usize = author_date.signed_duration_since(day_one).num_days().try_into().unwrap();
        let word = &answers[day];
        println!("[{day:3}] {} ({word})", first_guess.display());
    }

    Ok(())
}
