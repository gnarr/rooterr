use serde_json::Value;

#[derive(Debug, Clone)]
pub struct SeriesAdded {
    pub instance_name: String,
    pub sonarr_series_id: i64,
    pub title: Option<String>,
    pub year: Option<i64>,
    pub path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SeriesDetails {
    pub raw: Value,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeriesIds {
    pub tvdb_id: Option<i64>,
    pub tmdb_id: Option<i64>,
    pub imdb_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SeriesFolder {
    pub folder: String,
}

impl SeriesDetails {
    pub fn new(raw: Value) -> Self {
        Self { raw }
    }

    pub fn title(&self) -> Option<String> {
        json_string(&self.raw, "title")
    }

    pub fn year(&self) -> Option<i64> {
        json_i64(&self.raw, "year")
    }

    pub fn path(&self) -> Option<String> {
        json_string(&self.raw, "path")
    }

    pub fn ids(&self) -> SeriesIds {
        SeriesIds {
            tvdb_id: json_i64(&self.raw, "tvdbId"),
            tmdb_id: json_i64(&self.raw, "tmdbId"),
            imdb_id: json_string(&self.raw, "imdbId"),
        }
    }
}

pub fn json_i64(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(Value::as_i64).filter(|id| *id > 0)
}

pub fn json_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(ToOwned::to_owned)
}
