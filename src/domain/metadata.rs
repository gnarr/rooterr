use serde::Serialize;
use serde_json::Value;

use crate::domain::series::SeriesDetails;

const MAX_GENRES: usize = 20;
const MAX_KEYWORDS: usize = 24;
const MAX_RATINGS: usize = 12;
const MAX_SMALL_ARRAY: usize = 12;
const MAX_OVERVIEW_CHARS: usize = 1200;
const MAX_TAGLINE_CHARS: usize = 300;
const MAX_NAME_CHARS: usize = 120;

#[derive(Debug, Clone, Serialize)]
pub struct MetadataBundle {
    pub sonarr: Value,
    pub tmdb: Option<Value>,
    pub tmdb_error: Option<String>,
    pub tvdb: Option<Value>,
    pub tvdb_error: Option<String>,
}

impl MetadataBundle {
    pub fn new(series: SeriesDetails) -> Self {
        Self {
            sonarr: series.raw,
            tmdb: None,
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        }
    }

    pub fn classification_metadata(&self) -> ClassificationMetadata {
        ClassificationMetadata {
            sonarr: compact_sonarr(&self.sonarr),
            tmdb: self.tmdb.as_ref().map(compact_tmdb),
            tmdb_error: self.tmdb_error.clone(),
            tvdb: self.tvdb.as_ref().map(compact_tvdb),
            tvdb_error: self.tvdb_error.clone(),
        }
    }
}

impl ClassificationMetadata {
    pub fn has_explicit_kids_evidence(&self) -> bool {
        std::iter::once(&self.sonarr)
            .chain(self.tmdb.iter())
            .chain(self.tvdb.iter())
            .any(CompactSeriesMetadata::has_explicit_kids_evidence)
    }

    pub fn has_explicit_talk_show_evidence(&self) -> bool {
        std::iter::once(&self.sonarr)
            .chain(self.tmdb.iter())
            .chain(self.tvdb.iter())
            .any(CompactSeriesMetadata::has_explicit_talk_show_evidence)
    }

    pub fn has_explicit_documentary_evidence(&self) -> bool {
        std::iter::once(&self.sonarr)
            .chain(self.tmdb.iter())
            .chain(self.tvdb.iter())
            .any(CompactSeriesMetadata::has_explicit_documentary_evidence)
    }

    pub fn has_explicit_miniseries_evidence(&self) -> bool {
        std::iter::once(&self.sonarr)
            .chain(self.tmdb.iter())
            .chain(self.tvdb.iter())
            .any(CompactSeriesMetadata::has_explicit_miniseries_evidence)
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ClassificationMetadata {
    pub sonarr: CompactSeriesMetadata,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmdb: Option<CompactSeriesMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmdb_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tvdb: Option<CompactSeriesMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tvdb_error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct CompactSeriesMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tagline: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub genres: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub series_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_language: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub origin_countries: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub production_countries: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub certification: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub content_ratings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub networks: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_aired: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_aired: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number_of_seasons: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number_of_episodes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rating_value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rating_votes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vote_average: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vote_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imdb_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tvdb_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmdb_id: Option<i64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

impl CompactSeriesMetadata {
    fn has_explicit_kids_evidence(&self) -> bool {
        self.genres
            .iter()
            .chain(self.keywords.iter())
            .chain(self.tags.iter())
            .any(|value| is_explicit_kids_label(value))
            || self.content_ratings.iter().any(|value| {
                let normalized = value.trim().to_ascii_lowercase();
                let rating = normalized
                    .rsplit_once(": ")
                    .map_or(normalized.as_str(), |(_, rating)| rating);

                rating == "tv-y" || rating == "tv-y7" || rating == "tv-y7-fv"
            })
    }

    fn has_explicit_talk_show_evidence(&self) -> bool {
        self.series_type
            .as_deref()
            .is_some_and(is_explicit_talk_show_label)
            || self
                .genres
                .iter()
                .chain(self.keywords.iter())
                .chain(self.tags.iter())
                .any(|value| is_explicit_talk_show_label(value))
    }

    fn has_explicit_documentary_evidence(&self) -> bool {
        self.series_type
            .as_deref()
            .is_some_and(is_explicit_documentary_label)
            || self
                .genres
                .iter()
                .chain(self.keywords.iter())
                .chain(self.tags.iter())
                .any(|value| is_explicit_documentary_label(value))
    }

    fn has_explicit_miniseries_evidence(&self) -> bool {
        self.series_type
            .as_deref()
            .is_some_and(is_explicit_miniseries_label)
            || self
                .genres
                .iter()
                .chain(self.keywords.iter())
                .chain(self.tags.iter())
                .any(|value| is_explicit_miniseries_label(value))
    }
}

fn is_explicit_kids_label(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "children" | "children's" | "childrens" | "kids" | "kids' tv" | "kids tv"
    )
}

fn is_explicit_talk_show_label(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "talk" | "talk show" | "talk shows" | "talkshow" | "talkshows"
    )
}

fn is_explicit_documentary_label(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "documentary" | "documentaries" | "docuseries" | "docu-series"
    )
}

fn is_explicit_miniseries_label(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "mini-series" | "miniseries" | "mini series" | "limited series"
    )
}

fn compact_sonarr(value: &Value) -> CompactSeriesMetadata {
    CompactSeriesMetadata {
        title: string_path(value, &["title"]).map(|value| truncate_text(&value, MAX_NAME_CHARS)),
        year: int_path(value, &["year"]),
        overview: string_path(value, &["overview"])
            .map(|value| truncate_text(&value, MAX_OVERVIEW_CHARS)),
        genres: strings_from_array(value, &["genres"], &[], MAX_GENRES),
        series_type: string_path(value, &["seriesType"]),
        language: string_path(value, &["language"])
            .or_else(|| string_path(value, &["language", "name"])),
        original_language: string_path(value, &["originalLanguage"])
            .or_else(|| string_path(value, &["originalLanguage", "name"])),
        certification: string_path(value, &["certification"]),
        network: string_path(value, &["network"]),
        runtime: int_path(value, &["runtime"]),
        status: string_path(value, &["status"]),
        first_aired: string_path(value, &["firstAired"]),
        last_aired: string_path(value, &["lastAired"]),
        number_of_seasons: int_path(value, &["statistics", "seasonCount"]),
        number_of_episodes: int_path(value, &["statistics", "totalEpisodeCount"]),
        rating_value: float_path(value, &["ratings", "value"]),
        rating_votes: int_path(value, &["ratings", "votes"]),
        imdb_id: string_path(value, &["imdbId"]),
        tvdb_id: int_path(value, &["tvdbId"]),
        tmdb_id: int_path(value, &["tmdbId"]),
        tags: strings_from_array(value, &["tags"], &[], MAX_SMALL_ARRAY),
        ..CompactSeriesMetadata::default()
    }
}

fn compact_tmdb(value: &Value) -> CompactSeriesMetadata {
    CompactSeriesMetadata {
        title: string_path(value, &["name"])
            .or_else(|| string_path(value, &["title"]))
            .map(|value| truncate_text(&value, MAX_NAME_CHARS)),
        overview: string_path(value, &["overview"])
            .map(|value| truncate_text(&value, MAX_OVERVIEW_CHARS)),
        tagline: string_path(value, &["tagline"])
            .map(|value| truncate_text(&value, MAX_TAGLINE_CHARS)),
        genres: strings_from_array(value, &["genres"], &["name"], MAX_GENRES),
        series_type: string_path(value, &["type"]),
        original_language: string_path(value, &["original_language"]),
        origin_countries: strings_from_array(value, &["origin_country"], &[], MAX_SMALL_ARRAY),
        production_countries: strings_from_array(
            value,
            &["production_countries"],
            &["name"],
            MAX_SMALL_ARRAY,
        ),
        content_ratings: tmdb_content_ratings(value),
        networks: strings_from_array(value, &["networks"], &["name"], MAX_SMALL_ARRAY),
        status: string_path(value, &["status"]),
        first_aired: string_path(value, &["first_air_date"]),
        last_aired: string_path(value, &["last_air_date"]),
        number_of_seasons: int_path(value, &["number_of_seasons"]),
        number_of_episodes: int_path(value, &["number_of_episodes"]),
        vote_average: float_path(value, &["vote_average"]),
        vote_count: int_path(value, &["vote_count"]),
        imdb_id: string_path(value, &["external_ids", "imdb_id"]),
        tvdb_id: int_path(value, &["external_ids", "tvdb_id"]),
        tmdb_id: int_path(value, &["id"]),
        keywords: strings_from_array(value, &["keywords", "results"], &["name"], MAX_KEYWORDS),
        ..CompactSeriesMetadata::default()
    }
}

fn compact_tvdb(value: &Value) -> CompactSeriesMetadata {
    let extended = path(value, &["extended", "data"])
        .or_else(|| path(value, &["data"]))
        .unwrap_or(value);
    let translation = path(value, &["translation", "data"]);

    CompactSeriesMetadata {
        title: string_path(extended, &["name"])
            .or_else(|| string_path(extended, &["seriesName"]))
            .or_else(|| translation.and_then(|value| string_path(value, &["name"])))
            .map(|value| truncate_text(&value, MAX_NAME_CHARS)),
        year: int_path(extended, &["year"]),
        overview: translation
            .and_then(|value| string_path(value, &["overview"]))
            .or_else(|| string_path(extended, &["overview"]))
            .map(|value| truncate_text(&value, MAX_OVERVIEW_CHARS)),
        genres: strings_from_array(extended, &["genres"], &["name"], MAX_GENRES),
        series_type: string_path(extended, &["type"])
            .or_else(|| string_path(extended, &["type", "name"])),
        language: string_path(extended, &["language"])
            .or_else(|| string_path(extended, &["language", "name"])),
        original_language: string_path(extended, &["originalLanguage"])
            .or_else(|| string_path(extended, &["originalLanguage", "name"])),
        origin_countries: strings_from_array(extended, &["originalCountry"], &[], MAX_SMALL_ARRAY),
        content_ratings: tvdb_content_ratings(extended),
        network: string_path(extended, &["latestNetwork"])
            .or_else(|| string_path(extended, &["latestNetwork", "name"]))
            .or_else(|| string_path(extended, &["network"]))
            .or_else(|| string_path(extended, &["network", "name"])),
        networks: strings_from_array(extended, &["companies"], &["name"], MAX_SMALL_ARRAY),
        status: string_path(extended, &["status"])
            .or_else(|| string_path(extended, &["status", "name"])),
        first_aired: string_path(extended, &["firstAired"]),
        last_aired: string_path(extended, &["lastAired"]),
        rating_value: float_path(extended, &["score"]),
        tvdb_id: int_path(extended, &["id"]),
        aliases: strings_from_array(extended, &["aliases"], &["name"], MAX_SMALL_ARRAY),
        tags: strings_from_array(extended, &["tags"], &["name"], MAX_SMALL_ARRAY),
        ..CompactSeriesMetadata::default()
    }
}

fn tmdb_content_ratings(value: &Value) -> Vec<String> {
    let mut ratings = Vec::new();
    if let Some(values) = path(value, &["content_ratings", "results"]).and_then(Value::as_array) {
        for item in values {
            let rating = string_path(item, &["rating"]);
            if rating.is_none() {
                continue;
            }
            let country = string_path(item, &["iso_3166_1"]);
            let label = match (country, rating) {
                (Some(country), Some(rating)) => format!("{country}: {rating}"),
                (_, Some(rating)) => rating,
                _ => continue,
            };
            push_unique(&mut ratings, label, MAX_RATINGS);
        }
    }
    ratings
}

fn tvdb_content_ratings(value: &Value) -> Vec<String> {
    let mut ratings = Vec::new();
    for path_parts in [["contentRatings"].as_slice(), ["ratings"].as_slice()] {
        if let Some(values) = path(value, path_parts).and_then(Value::as_array) {
            for item in values {
                if let Some(rating) = string_from_value(item)
                    .or_else(|| string_path(item, &["name"]))
                    .or_else(|| string_path(item, &["rating"]))
                {
                    push_unique(&mut ratings, rating, MAX_RATINGS);
                }
            }
        }
    }
    ratings
}

fn strings_from_array(
    value: &Value,
    path_parts: &[&str],
    item_key: &[&str],
    max: usize,
) -> Vec<String> {
    let mut out = Vec::new();
    let Some(values) = path(value, path_parts) else {
        return out;
    };

    match values {
        Value::Array(values) => {
            for item in values {
                let value = if item_key.is_empty() {
                    string_from_value(item)
                } else {
                    string_path(item, item_key).or_else(|| string_from_value(item))
                };
                if let Some(value) = value {
                    push_unique(&mut out, value, max);
                }
            }
        }
        other => {
            if let Some(value) = string_from_value(other) {
                push_unique(&mut out, value, max);
            }
        }
    }

    out
}

fn push_unique(out: &mut Vec<String>, value: String, max: usize) {
    if out.len() >= max {
        return;
    }

    let value = truncate_text(&value, MAX_NAME_CHARS);
    if value.is_empty()
        || out
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&value))
    {
        return;
    }

    out.push(value);
}

fn path<'a>(value: &'a Value, path_parts: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for part in path_parts {
        current = current.get(*part)?;
    }
    Some(current)
}

fn string_path(value: &Value, path_parts: &[&str]) -> Option<String> {
    path(value, path_parts).and_then(string_from_value)
}

fn string_from_value(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.trim().to_string()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
    .filter(|value| !value.is_empty())
}

fn int_path(value: &Value, path_parts: &[&str]) -> Option<i64> {
    path(value, path_parts).and_then(|value| {
        value
            .as_i64()
            .or_else(|| value.as_u64().and_then(|value| value.try_into().ok()))
            .or_else(|| value.as_f64().map(|value| value as i64))
    })
}

fn float_path(value: &Value, path_parts: &[&str]) -> Option<f64> {
    path(value, path_parts).and_then(|value| {
        value
            .as_f64()
            .filter(|value| value.is_finite())
            .or_else(|| value.as_i64().map(|value| value as f64))
            .or_else(|| value.as_u64().map(|value| value as f64))
    })
}

fn truncate_text(value: &str, max_chars: usize) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }

    trimmed.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn compact_metadata_keeps_classification_signals() {
        let metadata = large_metadata_bundle();
        let compact = metadata.classification_metadata();

        assert_eq!(compact.sonarr.title.as_deref(), Some("Baywatch"));
        assert_eq!(compact.sonarr.genres, vec!["Action", "Drama"]);
        assert!(compact.tmdb.as_ref().expect("tmdb").keywords.starts_with(&[
            "sea".to_string(),
            "beach".to_string(),
            "lifeguard".to_string()
        ]));
        assert_eq!(
            compact
                .tmdb
                .as_ref()
                .expect("tmdb")
                .content_ratings
                .first()
                .map(String::as_str),
            Some("US: TV-PG")
        );
        assert_eq!(
            compact.tmdb.as_ref().expect("tmdb").networks,
            vec!["NBC", "Syndication"]
        );
    }

    #[test]
    fn compact_metadata_omits_large_low_value_provider_payloads() {
        let metadata = large_metadata_bundle();
        let compact_json =
            serde_json::to_string(&metadata.classification_metadata()).expect("compact json");

        assert!(compact_json.len() < 12_000);
        assert!(!compact_json.contains("aggregate_credits"));
        assert!(!compact_json.contains("profile_path"));
        assert!(!compact_json.contains("poster_path"));
        assert!(!compact_json.contains("\"seasons\""));
        assert!(!compact_json.contains("Season 1"));
        assert!(!compact_json.contains("remoteUrl"));
    }

    #[test]
    fn compact_metadata_caps_and_deduplicates_arrays() {
        let metadata = large_metadata_bundle();
        let compact = metadata.classification_metadata();
        let tmdb = compact.tmdb.expect("tmdb");

        assert!(tmdb.keywords.len() <= MAX_KEYWORDS);
        assert!(tmdb.genres.len() <= MAX_GENRES);
        assert_eq!(
            tmdb.keywords
                .iter()
                .filter(|keyword| keyword.eq_ignore_ascii_case("beach"))
                .count(),
            1
        );
    }

    #[test]
    fn compact_metadata_truncates_long_text_fields_by_field_type() {
        let metadata = MetadataBundle {
            sonarr: json!({
                "title": "T".repeat(200),
                "overview": "O".repeat(2000)
            }),
            tmdb: Some(json!({
                "name": "M".repeat(200),
                "tagline": "G".repeat(500)
            })),
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        };
        let compact = metadata.classification_metadata();

        assert_eq!(compact.sonarr.title.expect("title").chars().count(), 120);
        assert_eq!(
            compact.sonarr.overview.expect("overview").chars().count(),
            1200
        );
        assert_eq!(
            compact
                .tmdb
                .expect("tmdb")
                .tagline
                .expect("tagline")
                .chars()
                .count(),
            300
        );
    }

    #[test]
    fn compact_metadata_requires_explicit_kids_evidence() {
        let bluey = MetadataBundle {
            sonarr: json!({
                "title": "Bluey",
                "genres": ["Animation", "Children", "Comedy", "Family"]
            }),
            tmdb: Some(json!({
                "name": "Bluey",
                "keywords": { "results": [{ "name": "kids" }] }
            })),
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        };
        let reality_series = MetadataBundle {
            sonarr: json!({
                "title": "90 Day Fiancé",
                "genres": ["Reality", "Romance"],
                "overview": "International couples overcome cultural barriers and family drama."
            }),
            tmdb: Some(json!({
                "name": "90 Day Fiancé",
                "type": "Reality",
                "genres": [{ "name": "Reality" }]
            })),
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        };

        assert!(bluey.classification_metadata().has_explicit_kids_evidence());
        assert!(
            !reality_series
                .classification_metadata()
                .has_explicit_kids_evidence()
        );
    }

    #[test]
    fn compact_metadata_detects_explicit_talk_show_evidence() {
        let letterman = MetadataBundle {
            sonarr: json!({
                "title": "Late Show with David Letterman",
                "genres": ["Comedy", "Talk Show"],
                "seriesType": "daily"
            }),
            tmdb: Some(json!({
                "name": "Late Show with David Letterman",
                "type": "Talk Show",
                "genres": [{ "name": "Talk" }, { "name": "Comedy" }]
            })),
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        };
        let comedy = MetadataBundle {
            sonarr: json!({
                "title": "30 Rock",
                "genres": ["Comedy"],
                "seriesType": "standard"
            }),
            tmdb: Some(json!({
                "name": "30 Rock",
                "type": "Scripted",
                "genres": [{ "name": "Comedy" }]
            })),
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        };

        assert!(
            letterman
                .classification_metadata()
                .has_explicit_talk_show_evidence()
        );
        assert!(
            !comedy
                .classification_metadata()
                .has_explicit_talk_show_evidence()
        );
    }

    #[test]
    fn compact_metadata_requires_explicit_documentary_evidence() {
        let documentary = MetadataBundle {
            sonarr: json!({
                "title": "Planet Earth",
                "genres": ["Documentary"]
            }),
            tmdb: Some(json!({
                "name": "Planet Earth",
                "type": "Documentary",
                "genres": [{ "name": "Documentary" }]
            })),
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        };
        let historical_drama = MetadataBundle {
            sonarr: json!({
                "title": "Band of Brothers",
                "genres": ["Drama", "History", "Mini-Series", "War"],
                "overview": "Based on interviews with survivors and soldiers' journals."
            }),
            tmdb: Some(json!({
                "name": "Band of Brothers",
                "type": "Miniseries",
                "genres": [{ "name": "Drama" }, { "name": "War & Politics" }],
                "keywords": { "results": [{ "name": "historical drama" }] }
            })),
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        };

        assert!(
            documentary
                .classification_metadata()
                .has_explicit_documentary_evidence()
        );
        assert!(
            !historical_drama
                .classification_metadata()
                .has_explicit_documentary_evidence()
        );
    }

    #[test]
    fn compact_metadata_detects_explicit_miniseries_evidence() {
        let miniseries = MetadataBundle {
            sonarr: json!({
                "title": "Band of Brothers",
                "genres": ["Drama", "Mini-Series", "War"]
            }),
            tmdb: Some(json!({
                "name": "Band of Brothers",
                "type": "Miniseries"
            })),
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        };
        let scripted = MetadataBundle {
            sonarr: json!({
                "title": "Baywatch",
                "genres": ["Action", "Drama"],
                "seriesType": "standard"
            }),
            tmdb: Some(json!({
                "name": "Baywatch",
                "type": "Scripted"
            })),
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        };

        assert!(
            miniseries
                .classification_metadata()
                .has_explicit_miniseries_evidence()
        );
        assert!(
            !scripted
                .classification_metadata()
                .has_explicit_miniseries_evidence()
        );
    }

    fn large_metadata_bundle() -> MetadataBundle {
        let huge_people = (0..300)
            .map(|index| {
                json!({
                    "name": format!("Person {index}"),
                    "profile_path": format!("/profile-{index}.jpg"),
                    "roles": [{ "character": "Character", "episode_count": index }]
                })
            })
            .collect::<Vec<_>>();
        let huge_seasons = (0..50)
            .map(|index| {
                json!({
                    "name": format!("Season {index}"),
                    "poster_path": format!("/season-{index}.jpg"),
                    "episodes": [{ "name": "Episode", "overview": "Episode details" }]
                })
            })
            .collect::<Vec<_>>();
        let keywords = (0..40)
            .map(|index| {
                if index == 3 {
                    json!({ "name": "beach" })
                } else {
                    json!({ "name": format!("keyword-{index}") })
                }
            })
            .collect::<Vec<_>>();
        let mut keyword_values = vec![
            json!({ "name": "sea" }),
            json!({ "name": "beach" }),
            json!({ "name": "lifeguard" }),
            json!({ "name": "beach" }),
        ];
        keyword_values.extend(keywords);

        MetadataBundle {
            sonarr: json!({
                "title": "Baywatch",
                "year": 1989,
                "overview": "Lifeguards protect crowded Los Angeles beaches.",
                "genres": ["Action", "Drama"],
                "seriesType": "standard",
                "originalLanguage": { "name": "English" },
                "certification": "TV-PG",
                "network": "Syndication",
                "runtime": 45,
                "status": "ended",
                "firstAired": "1989-09-22T00:00:00Z",
                "lastAired": "2001-05-14T00:00:00Z",
                "ratings": { "value": 5.5, "votes": 32932 },
                "statistics": { "seasonCount": 11, "totalEpisodeCount": 243 },
                "imdbId": "tt0096542",
                "tvdbId": 70874,
                "tmdbId": 4386,
                "images": [{ "remoteUrl": "https://example.test/poster.jpg" }]
            }),
            tmdb: Some(json!({
                "id": 4386,
                "name": "Baywatch",
                "overview": "Join the Baywatch lifeguards on their adventures.",
                "tagline": "Always ready.",
                "type": "Scripted",
                "genres": [{ "name": "Drama" }, { "name": "Action & Adventure" }],
                "keywords": {
                    "results": keyword_values
                },
                "content_ratings": {
                    "results": [{ "iso_3166_1": "US", "rating": "TV-PG" }]
                },
                "networks": [{ "name": "NBC" }, { "name": "Syndication" }],
                "origin_country": ["US"],
                "production_countries": [{ "name": "United States of America" }],
                "number_of_seasons": 11,
                "number_of_episodes": 242,
                "vote_average": 5.987,
                "vote_count": 535,
                "external_ids": {
                    "imdb_id": "tt0096542",
                    "tvdb_id": 70874
                },
                "aggregate_credits": { "cast": huge_people },
                "seasons": huge_seasons,
                "poster_path": "/poster.jpg",
                "backdrop_path": "/backdrop.jpg"
            })),
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        }
    }
}
