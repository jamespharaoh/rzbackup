use std::fs;
use std::path::Path;
use std::path::PathBuf;

use rand;
use rand::Rng;

use rustc_serialize::hex::ToHex;

use ::Repository;
use ::TempFileManager;
use ::misc::*;
use ::zbackup::data::*;
use ::zbackup::write::*;

pub fn scan_index_files <
	RepositoryPath: AsRef <Path>,
> (
	repository_path: RepositoryPath,
) -> Result <Vec <(String, u64)>, String> {

	let repository_path =
		repository_path.as_ref ();

	let mut indexes_and_sizes: Vec <(String, u64)> =
		Vec::new ();

	// read directory

	for dir_entry_result in (

		io_result (
			fs::read_dir (
				repository_path.join (
					"index")))

	) ? {

		let dir_entry = (

			io_result (
				dir_entry_result)

		) ?;

		let file_name =
			dir_entry.file_name ();

		let index_name =
			file_name.to_str ().unwrap ().to_owned ();

		let index_metadata = (

			io_result (
				fs::metadata (
					dir_entry.path ()))

		) ?;

		indexes_and_sizes.push (
			(
				index_name,
				index_metadata.len (),
			)
		);

	}

	// return

	Ok (indexes_and_sizes)

}

pub fn scan_backup_files <
	RepositoryPath: AsRef <Path>,
> (
	repository_path: RepositoryPath,
) -> Result <Vec <PathBuf>, String> {

	let repository_path =
		repository_path.as_ref ();

	let mut backup_files: Vec <PathBuf> =
		Vec::new ();

	let backups_root =
		repository_path.join (
			"backups");

	scan_backup_files_real (
		& mut backup_files,
		& backups_root,
		& PathBuf::new (),
	) ?;

	Ok (backup_files)

}

fn scan_backup_files_real (
	backup_files: & mut Vec <PathBuf>,
	backups_root: & Path,
	directory: & Path,
) -> Result <(), String> {

	for dir_entry_result in (
		io_result (
			fs::read_dir (
				backups_root.join (
					directory)))
	) ? {

		let dir_entry = (
			io_result (
				dir_entry_result)
		) ?;

		let entry_metadata = (
			io_result (
				fs::metadata (
					dir_entry.path ()))
		) ?;

		if entry_metadata.is_dir () {

			scan_backup_files_real (
				backup_files,
				backups_root,
				& directory.join (
					dir_entry.file_name ()),
			) ?;

		} else if entry_metadata.is_file () {

			backup_files.push (
				directory.join (
					dir_entry.file_name ()));

		} else {

			panic! (
				"Don't know how to handle {:?}: {}",
				entry_metadata.file_type (),
				dir_entry.path ().to_string_lossy ());

		}

	}

	// return

	Ok (())

}

pub fn scan_bundle_files <
	RepositoryPath: AsRef <Path>,
> (
	repository_path: RepositoryPath,
) -> Result <Vec <String>, String> {

	let repository_path =
		repository_path.as_ref ();

	let mut bundle_files: Vec <String> =
		Vec::new ();

	for prefix in (0 .. 256).map (
		|byte| [ byte as u8 ].to_hex ()
	) {

		let bundles_directory =
			repository_path
				.join ("bundles")
				.join (prefix);

		if ! bundles_directory.exists () {
			continue;
		}

		for dir_entry_result in (
			io_result (
				fs::read_dir (
					bundles_directory))
		) ? {

			let dir_entry = (
				io_result (
					dir_entry_result)
			) ?;

			bundle_files.push (
				dir_entry.file_name ().to_str ().unwrap ().to_owned ());

		}

	}

	Ok (bundle_files)

}

pub fn flush_index_entries (
	repository: & Repository,
	temp_files: & mut TempFileManager,
	entries_buffer: & mut Vec <IndexEntry>,
) -> Result <(), String> {

	let new_index_bytes: Vec <u8> =
		rand::thread_rng ()
			.gen_iter::<u8> ()
			.take (24)
			.collect ();

	let new_index_name: String =
		new_index_bytes.to_hex ();

	let new_index_path =
		repository.path ()
			.join ("index")
			.join (new_index_name);

	let new_index_file =
		Box::new (
			temp_files.create (
				new_index_path,
			) ?
		);

	write_index (
		new_index_file,
		repository.encryption_key (),
		& entries_buffer,
	) ?;

	entries_buffer.clear ();

	Ok (())

}

// ex: noet ts=4 filetype=rust
