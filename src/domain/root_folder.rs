use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RootFolder {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RootFolderChoice {
    pub path: String,
    pub label: Option<String>,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct RootFolderHint {
    pub label: Option<String>,
    pub description: Option<String>,
}

pub fn join_series_path(root_folder_path: &str, series_folder: &str) -> String {
    let separator = if is_windows_path(root_folder_path) {
        "\\"
    } else {
        "/"
    };
    let root = root_folder_path.trim_end_matches(['/', '\\']);
    let folder = series_folder.trim_matches(['/', '\\']);

    if root.is_empty() {
        format!("{separator}{folder}")
    } else if folder.is_empty() {
        root.to_string()
    } else {
        format!("{root}{separator}{folder}")
    }
}

fn is_windows_path(path: &str) -> bool {
    path.contains('\\') || path.as_bytes().get(1) == Some(&b':')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_unix_series_path() {
        assert_eq!(
            join_series_path("/data/kids", "Bluey (2018)"),
            "/data/kids/Bluey (2018)"
        );
        assert_eq!(
            join_series_path("/data/kids/", "/Bluey (2018)/"),
            "/data/kids/Bluey (2018)"
        );
    }

    #[test]
    fn joins_windows_series_path() {
        assert_eq!(
            join_series_path(r"C:\Media\Kids", "Bluey (2018)"),
            r"C:\Media\Kids\Bluey (2018)"
        );
        assert_eq!(
            join_series_path(r"\\server\media\kids\", "Bluey (2018)"),
            r"\\server\media\kids\Bluey (2018)"
        );
    }
}
