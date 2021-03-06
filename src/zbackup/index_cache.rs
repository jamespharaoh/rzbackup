use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::sync::Arc;

use futures;
use futures::BoxFuture;
use futures::Future;
use futures_cpupool::CpuPool;

use num_cpus;

use output::Output;

use rustc_serialize::hex::ToHex;

use misc::*;
use zbackup::data::*;
use zbackup::disk_format::*;
use zbackup::repository_core::*;

/// The index cache loads and caches information from index files, namely a set
/// of mappings from chunk IDs to bundle IDs, along with the size of the chunk's
/// uncompressed data.

pub struct IndexCache {
	repository_core: Arc <RepositoryCore>,
	entries: Option <HashMap <ChunkId, IndexEntry>>,
}

#[ derive (Clone, Copy, Debug) ]
pub struct IndexEntry {
	bundle_id: BundleId,
	size: u64,
}

impl IndexEntry {

	#[ inline ]
	pub fn bundle_id (& self) -> BundleId {
		self.bundle_id
	}

	#[ inline ]
	pub fn size (& self) -> u64 {
		self.size
	}

}

type IndexLoadFuture =
	BoxFuture <
		(IndexId, Vec <(ChunkId, IndexEntry)>),
		(IndexId, String),
	>;

impl IndexCache {

	#[ inline ]
	pub fn new (
		repository_core: Arc <RepositoryCore>,
	) -> IndexCache {

		IndexCache {
			repository_core: repository_core,
			entries: None,
		}

	}

	#[ inline ]
	pub fn load_if_not_loaded (
		& mut self,
		output: & Output,
	) -> Result <(), String> {

		if self.entries.is_some () {

			Ok (())

		} else {

			self.load_impl (
				output,
			)

		}

	}

	#[ inline ]
	pub fn reload (
		& mut self,
		output: & Output,
	) -> Result <(), String> {

		self.load_impl (
			output,
		)

	}

	fn load_impl (
		& mut self,
		output: & Output,
	) -> Result <(), String> {

		let bundle_ids: Arc <HashSet <BundleId>> =
			Arc::new (
				self.scan_bundles (
					output,
				) ?
			);

		let index_ids =
			self.scan_indexes (
				output,
			) ?;

		self.entries = Some (
			self.load_indexes (
				output,
				bundle_ids.clone (),
				& index_ids,
			) ?
		);

		Ok (())

	}

	fn load_indexes (
		& self,
		output: & Output,
		bundle_ids: Arc <HashSet <BundleId>>,
		index_ids: & Vec <IndexId>,
	) -> Result <HashMap <ChunkId, IndexEntry>, String> {

		let output_job =
			output_job_start! (
				output,
				"Loading indexes");

		let num_indexes_total =
			index_ids.len () as u64;

		let mut num_indexes_loaded: u64 = 0;
		let mut num_indexes_error: u64 = 0;

		let mut index_futures: Vec <IndexLoadFuture> =
			Vec::new ();

		let num_threads =
			(num_cpus::get () - 1) * 7 / 3 + 1;

		let cpu_pool =
			CpuPool::new (
				num_threads);

		let mut index_ids_iter =
			index_ids.iter ();

		let mut all_entries =
			HashMap::new ();

		output.pause ();

		loop {

			output_job.progress (
				num_indexes_loaded + num_indexes_error,
				num_indexes_total);

			// start indexes loading

			while index_futures.len () < num_threads {

				if let Some (index_id) =
					index_ids_iter.next () {

					index_futures.push (
						Self::load_index_future (
							self.repository_core.clone (),
							output,
							& cpu_pool,
							bundle_ids.clone (),
							* index_id,
						)
					);

				} else {
					break;
				}

			}

			// process loaded indexes

			if index_futures.is_empty () {
				break;
			}

			output.unpause ();

			match futures::select_all (
				index_futures,
			).wait () {

				Ok ((
					(_index_id, index_entries),
					_task_index,
					remaining_index_futures,
				)) => {

					index_futures =
						remaining_index_futures;

					for (chunk_id, index_entry) in index_entries {

						all_entries.insert (
							chunk_id,
							index_entry,
						);

					}

					num_indexes_loaded += 1;

				},

				Err ((
					(index_id, error),
					_task_index,
					remaining_index_futures,
				)) => {

					index_futures =
						remaining_index_futures;

					output_message! (
						output,
						"Error loading index {}: {}",
						index_id,
						error);

					num_indexes_error += 1;

				},

			}

			output.pause ();

		}

		output.unpause ();

		if num_indexes_error > 0 {

			output_job_replace! (
				output_job,
				"Loaded {} indexes with {} errors",
				num_indexes_loaded,
				num_indexes_error);

		} else {

			output_job_replace! (
				output_job,
				"Loaded {} indexes",
				num_indexes_loaded);

		}

		// return

		Ok (all_entries)

	}

	fn load_index_future (
		repository_core: Arc <RepositoryCore>,
		output: & Output,
		cpu_pool: & CpuPool,
		bundle_ids: Arc <HashSet <BundleId>>,
		index_id: IndexId,
	) -> IndexLoadFuture {

		let index_path =
			repository_core.index_path (
				index_id);

		let output_job =
			output_job_start! (
				output,
				"Loading index {}",
				index_id);

		cpu_pool.spawn_fn (move || {

			let index_data =
				string_result_with_prefix (
					|| format! (
						"Error loading index {}",
						index_id),
					index_read_path (
						index_path,
						repository_core.encryption_key ()),
				) ?;

			let mut raw_entries: Vec <(ChunkId, IndexEntry)> =
				Vec::new ();

			for RawIndexEntry {
				index_bundle_header,
				bundle_info,
			} in index_data {

				let bundle_id =
					index_bundle_header.bundle_id ();

				if ! bundle_ids.contains (& bundle_id) {
					continue;
				}

				for chunk in bundle_info.chunks () {

					raw_entries.push ((
						chunk.chunk_id (),
						IndexEntry {
							bundle_id: bundle_id,
							size: chunk.size () as u64,
						},
					));

				}

			}

			Ok ((index_id, raw_entries))

		}).map_err (
			move |error|

			(index_id, error)

		).then (
			move |result| {

			output_job.remove ();

			result

		}).boxed ()

	}

	fn scan_bundles (
		& self,
		output: & Output,
	) -> Result <HashSet <BundleId>, String> {

		let output_job =
			output_job_start! (
				output,
				"Scanning bundles");

		let mut bundle_ids: HashSet <BundleId> =
			HashSet::new ();

		for prefix in (0 .. 256).map (
			|byte| [ byte as u8 ].to_hex ()
		) {

			let bundles_directory =
				self.repository_core.path ()
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

				match BundleId::parse (
					dir_entry.file_name ().to_str ().unwrap (),
				) {

					Ok (bundle_id) => {

						bundle_ids.insert (
							bundle_id);

					},

					Err (_) =>
						output.message_format (
							format_args! (
								"Ignoring invalid bundle name: {}",
								dir_entry.file_name ().to_string_lossy ())),

				}

			}

		}

		output_job_replace! (
			output_job,
			"Found {} bundle files",
			bundle_ids.len ());

		Ok (bundle_ids)

	}

	fn scan_indexes (
		& self,
		output: & Output,
	) -> Result <Vec <IndexId>, String> {

		let output_job =
			output_job_start! (
				output,
				"Scanning index files");

		let mut index_ids =
			Vec::new ();

		for dir_entry_or_error in (

			io_result (
				fs::read_dir (
					self.repository_core.path ()
						.join ("index")))

		) ? {

			let dir_entry =
				io_result (
					dir_entry_or_error,
				) ?;

			if let Some (index_filename) =
				dir_entry.file_name ().to_str () {

				if let Ok (index_id) =
					IndexId::parse (
						index_filename,
					) {

					index_ids.push (
						index_id);

				} else {

					output_message! (
						output,
						"Ignoring invalid index name: {}",
						index_filename);

				}

			} else {

				output_message! (
					output,
					"Ignoring invalid index name: {}",
					dir_entry.file_name ().to_string_lossy ());

			}

		}

		output_job_replace! (
			output_job,
			"Found {} index files",
			index_ids.len ());

		Ok (index_ids)

	}

	pub fn get (
		& self,
		chunk_id: & ChunkId,
	) -> Option <IndexEntry> {

		self.entries.as_ref ().unwrap ().get (
			chunk_id,
		).map (
			|index_entry|

			index_entry.to_owned ()

		)

	}

	pub fn loaded (& self) -> bool {
		self.entries.is_some ()
	}

}

// ex: noet ts=4 filetype=rust
