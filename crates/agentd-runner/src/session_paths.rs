use std::path::PathBuf;

const HOME_ROOT_DIR: &str = "/home";
const REPO_DIR_NAME: &str = "repo";

pub(crate) fn session_home_dir(profile_name: &str) -> PathBuf {
    PathBuf::from(HOME_ROOT_DIR).join(profile_name)
}

pub(crate) fn session_repo_dir(profile_name: &str) -> PathBuf {
    session_home_dir(profile_name).join(REPO_DIR_NAME)
}
