use anyhow::Result;
use tempfile::tempdir;

use crate::{
    app::{gb_repository, project_repository},
    deltas, projects, storage, users,
};

use super::project_file_change::Listener;

fn commit_all(repository: &git2::Repository) -> Result<git2::Oid> {
    let mut index = repository.index()?;
    index.add_all(&["."], git2::IndexAddOption::DEFAULT, None)?;
    index.write()?;
    let oid = index.write_tree()?;
    let signature = git2::Signature::now("test", "test@email.com").unwrap();
    let commit_oid = repository.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "some commit",
        &repository.find_tree(oid)?,
        &[&repository.find_commit(repository.refname_to_id("HEAD")?)?],
    )?;
    Ok(commit_oid)
}

fn test_repository() -> Result<git2::Repository> {
    let path = tempdir()?.path().to_str().unwrap().to_string();
    let repository = git2::Repository::init(&path)?;
    let mut index = repository.index()?;
    let oid = index.write_tree()?;
    let signature = git2::Signature::now("test", "test@email.com").unwrap();
    repository.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "Initial commit",
        &repository.find_tree(oid)?,
        &[],
    )?;
    Ok(repository)
}

fn test_project(repository: &git2::Repository) -> Result<projects::Project> {
    let project = projects::Project::from_path(
        repository
            .path()
            .parent()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string(),
    )?;
    Ok(project)
}

#[test]
fn test_register_existing_file() -> Result<()> {
    let repository = test_repository()?;
    let project = test_project(&repository)?;
    let project_repo = project_repository::Repository::open(&project)?;
    let gb_repo_path = tempdir()?.path().to_str().unwrap().to_string();
    let storage = storage::Storage::from_path(tempdir()?.path().to_path_buf());
    let user_store = users::Storage::new(storage.clone());
    let project_store = projects::Storage::new(storage);
    project_store.add_project(&project)?;
    let gb_repo = gb_repository::Repository::open(
        gb_repo_path,
        project.id.clone(),
        project_store.clone(),
        user_store,
    )?;
    let listener = Listener::new(project.id.clone(), project_store, &gb_repo);

    let file_path = std::path::Path::new("test.txt");
    std::fs::write(project_repo.root().join(file_path), "test")?;
    commit_all(&repository)?;

    std::fs::write(project_repo.root().join(file_path), "test2")?;
    listener.register(file_path)?;

    let raw_deltas = std::fs::read_to_string(gb_repo.deltas_path().join(file_path))?;
    let deltas: Vec<deltas::Delta> = serde_json::from_str(&raw_deltas)?;
    assert_eq!(deltas.len(), 1);
    assert_eq!(deltas[0].operations.len(), 1);
    assert_eq!(
        deltas[0].operations[0],
        deltas::Operation::Insert((4, "2".to_string())),
    );
    assert_eq!(
        std::fs::read_to_string(gb_repo.session_wd_path().join(file_path))?,
        "test2"
    );

    Ok(())
}

#[test]
fn test_register_new_file() -> Result<()> {
    let repository = test_repository()?;
    let project = test_project(&repository)?;
    let project_repo = project_repository::Repository::open(&project)?;
    let gb_repo_path = tempdir()?.path().to_str().unwrap().to_string();
    let storage = storage::Storage::from_path(tempdir()?.path().to_path_buf());
    let user_store = users::Storage::new(storage.clone());
    let project_store = projects::Storage::new(storage);
    project_store.add_project(&project)?;
    let gb_repo = gb_repository::Repository::open(
        gb_repo_path,
        project.id.clone(),
        project_store.clone(),
        user_store,
    )?;
    let listener = Listener::new(project.id.clone(), project_store, &gb_repo);

    let file_path = std::path::Path::new("test.txt");
    std::fs::write(project_repo.root().join(file_path), "test")?;

    listener.register(file_path)?;

    let raw_deltas = std::fs::read_to_string(gb_repo.deltas_path().join(file_path))?;
    let deltas: Vec<deltas::Delta> = serde_json::from_str(&raw_deltas)?;
    assert_eq!(deltas.len(), 1);
    assert_eq!(deltas[0].operations.len(), 1);
    assert_eq!(
        deltas[0].operations[0],
        deltas::Operation::Insert((0, "test".to_string())),
    );
    assert_eq!(
        std::fs::read_to_string(gb_repo.session_wd_path().join(file_path))?,
        "test"
    );

    Ok(())
}

#[test]
fn test_register_new_file_twice() -> Result<()> {
    let repository = test_repository()?;
    let project = test_project(&repository)?;
    let project_repo = project_repository::Repository::open(&project)?;
    let gb_repo_path = tempdir()?.path().to_str().unwrap().to_string();
    let storage = storage::Storage::from_path(tempdir()?.path().to_path_buf());
    let user_store = users::Storage::new(storage.clone());
    let project_store = projects::Storage::new(storage);
    project_store.add_project(&project)?;
    let gb_repo = gb_repository::Repository::open(
        gb_repo_path,
        project.id.clone(),
        project_store.clone(),
        user_store,
    )?;
    let listener = Listener::new(project.id.clone(), project_store, &gb_repo);

    let file_path = std::path::Path::new("test.txt");
    std::fs::write(project_repo.root().join(file_path), "test")?;
    listener.register(file_path)?;

    std::fs::write(project_repo.root().join(file_path), "test2")?;
    listener.register(file_path)?;

    let raw_deltas = std::fs::read_to_string(gb_repo.deltas_path().join(file_path))?;
    let deltas: Vec<deltas::Delta> = serde_json::from_str(&raw_deltas)?;
    assert_eq!(deltas.len(), 2);
    assert_eq!(deltas[0].operations.len(), 1);
    assert_eq!(
        deltas[0].operations[0],
        deltas::Operation::Insert((0, "test".to_string())),
    );
    assert_eq!(deltas[1].operations.len(), 1);
    assert_eq!(
        deltas[1].operations[0],
        deltas::Operation::Insert((4, "2".to_string())),
    );
    assert_eq!(
        std::fs::read_to_string(gb_repo.session_wd_path().join(file_path))?,
        "test2"
    );

    Ok(())
}
