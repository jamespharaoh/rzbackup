#![ allow (unused_parens) ]

extern crate num_cpus;

use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::LinkedList;
use std::fs;
use std::io::Cursor;
use std::io::Read;
use std::io::Write;
use std::ops::DerefMut;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use crypto::digest::Digest;
use crypto::sha1::Sha1;
use crypto::sha2::Sha256;

use futures;
use futures::BoxFuture;
use futures::Complete;
use futures::Future;

use futures_cpupool::CpuPool;

use lru_cache::LruCache;

use output::Output;

use protobuf::stream::CodedInputStream;

use rustc_serialize::hex::FromHex;
use rustc_serialize::hex::ToHex;

use misc::*;

use zbackup::crypto::*;
use zbackup::data::*;
use zbackup::proto;
use zbackup::randaccess::*;
use zbackup::read::*;
use zbackup::storage::*;

type MasterIndex = HashMap <BundleId, MasterIndexEntry>;
type ChunkMap = Arc <HashMap <ChunkId, ChunkData>>;
type ChunkCache = LruCache <ChunkId, ChunkData>;

#[ derive (Clone) ]
pub struct MasterIndexEntryData {
	pub bundle_id: BundleId,
	pub size: u64,
}

pub type MasterIndexEntry = Arc <MasterIndexEntryData>;

/// This controls the configuration of a repository, and is passed to the `open`
/// constructor.

#[ derive (Clone) ]
pub struct RepositoryConfig {
	pub max_uncompressed_memory_cache_entries: usize,
	pub max_compressed_memory_cache_entries: usize,
	pub max_compressed_filesystem_cache_entries: usize,
	pub max_threads: usize,
	pub filesystem_cache_path: String,
	pub work_jobs_total: usize, // deprecated and ignored
	pub work_jobs_batch: usize, // deprecated and ignored
}

struct RepositoryData {
	config: RepositoryConfig,
	path: PathBuf,
	storage_info: proto::StorageInfo,
	encryption_key: Option <EncryptionKey>,
}

type ChunkWaiter = Complete <Result <ChunkData, String>>;
type BundleWaiters = HashMap <ChunkId, Vec <ChunkWaiter>>;

type FutureChunkWaiter = Complete <BoxFuture <ChunkData, String>>;
type FutureBundleWaiters = HashMap <ChunkId, Vec <FutureChunkWaiter>>;

struct RepositoryState {
	master_index: Option <MasterIndex>,
	bundles_loading: HashMap <BundleId, BundleWaiters>,
	bundles_to_load: HashMap <BundleId, FutureBundleWaiters>,
	bundles_to_load_list: LinkedList <BundleId>,
}

/// This is the main struct which implements the ZBackup restore functionality.
/// It is multi-threaded, using a cpu pool internally, and it is fully thread
/// safe.

#[ derive (Clone) ]
pub struct Repository {
	data: Arc <RepositoryData>,
	state: Arc <Mutex <RepositoryState>>,
	cpu_pool: CpuPool,
	storage_manager: StorageManager,
}

impl Repository {

	/// Provides a default configuration for a Repository. This may be useful
	/// for some users of the library, although normally a custom configuration
	/// will be a better option.

	pub fn default_config () -> RepositoryConfig {

		RepositoryConfig {

			max_uncompressed_memory_cache_entries:
				MAX_UNCOMPRESSED_MEMORY_CACHE_ENTRIES,

			max_compressed_memory_cache_entries:
				MAX_COMPRESSED_MEMORY_CACHE_ENTRIES,

			max_compressed_filesystem_cache_entries:
				MAX_COMPRESSED_FILESYSTEM_CACHE_ENTRIES,

			max_threads:
				num_cpus::get (),

			filesystem_cache_path:
				FILESYSTEM_CACHE_PATH.to_owned (),

			work_jobs_total: 0, // deprecated and ignored
			work_jobs_batch: 0, // deprecated and ignored

		}

	}

	/// Constructs a new Repository from a configuration and a path, and an
	/// optional password file path.
	///
	/// This will read the repositories info file, and decrypt the encryption
	/// key using the password, if provided.

	pub fn open <
		RepositoryPath: AsRef <Path>,
		PasswordFilePath: AsRef <Path>,
	> (
		output: & Output,
		repository_config: RepositoryConfig,
		repository_path: RepositoryPath,
		password_file_path: Option <PasswordFilePath>,
	) -> Result <Repository, String> {

		let repository_path =
			repository_path.as_ref ();

		let password_file_path =
			password_file_path.as_ref ();

		// load info file

		output.status_format (
			format_args! (
				"Loading repository {} ...",
				repository_path.to_string_lossy ()));

		let storage_info = (

			read_storage_info (
				repository_path.join (
					"info"))

		) ?;

		// decrypt encryption key with password

		let encryption_key =
			if storage_info.has_encryption_key () {

			if password_file_path.is_none () {

				output.clear_status ();

				return Err (
					"Required password file not provided".to_string ());

			}

			match try! (
				decrypt_key (
					password_file_path.unwrap (),
					storage_info.get_encryption_key ())) {

				Some (key) =>
					Some (key),

				None => {

					output.clear_status ();

					return Err (
						"Incorrect password".to_string ());

				},

			}

		} else {

			if password_file_path.is_some () {

				output.clear_status ();

				return Err (
					"Unnecessary password file provided".to_string ());

			}

			None

		};

		output.status_done ();

		// create thread pool

		let cpu_pool =
			CpuPool::new (
				repository_config.max_threads + 1);

		// create storage manager

		let storage_manager =
			try! (

			StorageManager::new (
				repository_config.filesystem_cache_path.clone (),
				cpu_pool.clone (),
				repository_config.max_uncompressed_memory_cache_entries,
				repository_config.max_compressed_memory_cache_entries,
				repository_config.max_compressed_filesystem_cache_entries,
			)

		);

		// create data

		let repository_data =
			Arc::new (
				RepositoryData {

			config: repository_config,

			path: repository_path.to_owned (),
			storage_info: storage_info,
			encryption_key: encryption_key,

		});

		// create state

		let repository_state =
			Arc::new (
				Mutex::new (
					RepositoryState {

			master_index:
				None,

			bundles_loading:
				HashMap::new (),

			bundles_to_load:
				HashMap::new (),

			bundles_to_load_list:
				LinkedList::new (),

		}));

		// return

		Ok (Repository {

			data: repository_data,
			state: repository_state,
			cpu_pool: cpu_pool,
			storage_manager: storage_manager,

		})

	}

	/// Load the index files. This is not done automatically, but it will be
	/// done lazily when they are first needed. This function also implements a
	/// lazy loading pattern, and so no index files will be reloaded if it is
	/// called more than ones.
	///
	/// Apart from being used internally, this function is designed to be used
	/// by library users who want to eagerly load the indexes so that restore
	/// operations can begin more quickly. This would also allow errors when
	/// reading the index files to be caught more quickly and deterministically.

	pub fn load_indexes (
		& self,
		output: & Output,
	) -> Result <(), String> {

		let mut self_state =
			self.state.lock ().unwrap ();

		if self_state.master_index.is_some () {
			return Ok (());
		}

		self.load_indexes_real (
			self_state.deref_mut (),
			output)

	}

	/// Reload the index files. This forces the indexes to be reloaded, even if
	/// they have already been loaded. This should be called if new backups have
	/// been added to an already-open repository.

	pub fn reload_indexes (
		& self,
		output: & Output,
	) -> Result <(), String> {

		let mut self_state =
			self.state.lock ().unwrap ();

		self.load_indexes_real (
			self_state.deref_mut (),
			output)

	}

	fn load_indexes_real (
		& self,
		self_state: & mut RepositoryState,
		output: & Output,
	) -> Result <(), String> {

		struct IndexEntryData {
			chunk_id: ChunkId,
			bundle_id: BundleId,
			size: u64,
		};

		type IndexLoadResult =
			BoxFuture <
				Vec <IndexEntryData>,
				String,
			>;

		output.status (
			"Scanning bundles ...");

		let mut bundle_ids: HashSet <BundleId> =
			HashSet::new ();

		for prefix in (0 .. 256).map (
			|byte| [ byte as u8 ].to_hex ()
		) {

			let bundles_directory =
				self.data.path
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

				let file_name =
					dir_entry.file_name ().to_str ().unwrap ().to_owned ();

				let bundle_id =
					to_array_24 (
						& file_name.from_hex ().unwrap ());

				bundle_ids.insert (
					bundle_id);

			}

		}

		output.status_done ();

		output.message_format (
			format_args! (
				"Found {} bundle files",
				bundle_ids.len ()));

		let bundle_ids =
			Arc::new (
				bundle_ids);

		output.status (
			"Loading indexes ...");

		// start tasks to load each index

		let mut index_result_futures: Vec <IndexLoadResult> =
			Vec::new ();

		for dir_entry_or_error in (

			io_result (
				fs::read_dir (
					self.data.path.join (
						"index")))

		) ? {

			let dir_entry =
				try! (
					io_result (
						dir_entry_or_error));

			let file_name =
				dir_entry.file_name ();

			let index_name =
				file_name.to_str ().unwrap ().to_owned ();

			let self_clone =
				self.clone ();

			let bundle_ids =
				bundle_ids.clone ();

			index_result_futures.push (
				self.cpu_pool.spawn_fn (
					move || {

				let index = (

					string_result_with_prefix (
						|| format! (
							"Error loading index {}",
							index_name),
						read_index (
							self_clone.data.path
								.join ("index")
								.join (& index_name),
							self_clone.data.encryption_key))

				) ?;

				let mut entries: Vec <IndexEntryData> =
					Vec::new ();

				for (index_bundle_header, bundle_info) in index {

					let bundle_id =
						to_array_24 (
							index_bundle_header.get_id ());

					if ! bundle_ids.contains (& bundle_id) {
						continue;
					}

					for chunk_record in bundle_info.get_chunk_record () {

						entries.push (
							IndexEntryData {

							chunk_id:
								to_array_24 (
									chunk_record.get_id ()),

							bundle_id:
								bundle_id,

							size:
								chunk_record.get_size () as u64,

						});

					}

				}

				Ok (entries)

			}).boxed ());

		}

		let num_indexes =
			index_result_futures.len () as u64;

		// construct index as they complete

		let mut count: u64 = 0;
		let mut error_count: u64 = 0;

		let mut master_index: MasterIndex =
			HashMap::new ();

		for index_result_future in index_result_futures {

			match index_result_future.wait () {

				Ok (index_entries) => {

					for index_entry in index_entries {

						master_index.insert (

							index_entry.chunk_id,

							Arc::new (MasterIndexEntryData {
								bundle_id: index_entry.bundle_id,
								size: index_entry.size,
							}),

						);

					}

					count += 1;

				},

				Err (error) => {

					output.message (
						error);

					error_count += 1;

				},

			}

			if count & 0x3f == 0x3f {

				output.status_progress (
					count as u64,
					num_indexes as u64);

			}

		}

		output.status_done ();

		if error_count > 0 {

			output.message_format (
				format_args! (
					"{} index files not loaded due to errors",
					error_count));

		}

		// store the result and return

		self_state.master_index =
			Some (
				master_index);

		Ok (())

	}

	/// This will load a backup entirely into memory. The use of this function
	/// should probably be discouraged for most use cases, since backups could
	/// be extremely large.

	pub fn read_and_expand_backup (
		& self,
		output: & Output,
		backup_name: & str,
	) -> Result <(Vec <u8>, [u8; 32]), String> {

		try! (
			self.load_indexes (
				output));

		// load backup

		output.status_format (
			format_args! (
				"Loading backup {} ...",
				backup_name));

		let backup_info = (

			read_backup_file (
				self.data.path
					.join ("backups")
					.join (& backup_name [1 .. ]),
				self.data.encryption_key,
			).or_else (
				|error| {

					output.status_done ();

					Err (error)

				}
			)

		) ?;

		// expand backup data

		let mut input =
			Cursor::new (
				backup_info.get_backup_data ().to_owned ());

		for _iteration in 0 .. backup_info.get_iterations () {

			let mut temp_output: Cursor <Vec <u8>> =
				Cursor::new (
					Vec::new ());

			let mut sha1_digest =
				Sha1::new ();

			self.follow_instructions (
				& mut input,
				& mut temp_output,
				& mut sha1_digest,
				& |count| {
					if count & 0xf == 0xf {
						output.status_tick ();
					}
				},
			) ?;

			input =
				Cursor::new (
					temp_output.into_inner ());

		}

		output.status_done ();

		Ok (
			(
				input.into_inner (),
				to_array_32 (
					backup_info.get_sha256 ()),
			)
		)

	}

	/// This function will restore a named backup, writing it to the provided
	/// implementation of the `Write` trait.

	pub fn restore (
		& self,
		output: & Output,
		backup_name: & str,
		target: & mut Write,
	) -> Result <(), String> {

		if backup_name.is_empty () {

			return Err (
				"Backup name must not be empty".to_string ());

		}

		if backup_name.chars ().next ().unwrap () != '/' {

			return Err (
				"Backup name must begin with '/'".to_string ());

		}

		let (input_bytes, checksum) =
			self.read_and_expand_backup (
				output,
				backup_name,
			) ?;

		let mut input =
			Cursor::new (
				input_bytes);

		output.status_format (
			format_args! (
				"Restoring {}",
				backup_name));

		// restore backup

		let mut sha256_sum =
			Sha256::new ();

		self.follow_instructions (
			& mut input,
			target,
			& mut sha256_sum,
			& |count| {
				if count & 0x7f == 0x00 {
					output.status_tick ();
				}
			},
		) ?;

		// verify checksum

		let mut sha256_sum_bytes: [u8; 32] =
			[0u8; 32];

		sha256_sum.result (
			& mut sha256_sum_bytes);

		if checksum != sha256_sum_bytes {

			return Err (
				format! (
					"Expected sha256 checksum {} but calculated {}",
					checksum.to_hex (),
					sha256_sum_bytes.to_hex ()));

		}

		// done

		output.status_done ();

		Ok (())

	}

	#[ doc (hidden) ]
	pub fn restore_test (
		& self,
		output: & Output,
		backup_name: & str,
		target: & mut Write,
	) -> Result <(), String> {

		output.status_format (
			format_args! (
				"Restoring {}",
				backup_name));

		let mut input =
			try! (
				RandomAccess::new (
					output,
					self,
					backup_name));

		let mut buffer: Vec <u8> =
			vec! [0u8; BUFFER_SIZE];

		// restore backup

		loop {

			let bytes_read =
				try! (
					io_result (
						input.read (
							& mut buffer)));

			if bytes_read == 0 {
				break;
			}

			try! (
				io_result (
					target.write (
						& buffer [
							0 .. bytes_read ])));

		}

		output.status_done ();

		Ok (())

	}

	fn follow_instruction_async_async (
		& self,
		backup_instruction: & proto::BackupInstruction,
	) -> BoxFuture <BoxFuture <ChunkData, String>, String> {

		if backup_instruction.has_chunk_to_emit ()
		&& backup_instruction.has_bytes_to_emit () {

			let chunk_id =
				to_array_24 (
					backup_instruction.get_chunk_to_emit ());

			let backup_instruction_bytes_to_emit =
				backup_instruction.get_bytes_to_emit ().to_vec ();

			self.get_chunk_async_async (
				chunk_id,
			).map (
				move |chunk_data_future|

				chunk_data_future.map (
					move |chunk_data|

					Arc::new (
						chunk_data.iter ().map (
							move |& value| value
						).chain (
							backup_instruction_bytes_to_emit.into_iter ()
						).collect ())

				).boxed ()

			).boxed ()

		} else if backup_instruction.has_chunk_to_emit () {

			let chunk_id =
				to_array_24 (
					backup_instruction.get_chunk_to_emit ());

			self.get_chunk_async_async (
				chunk_id,
			)

		} else if backup_instruction.has_bytes_to_emit () {

			futures::done (Ok (
				futures::done (Ok (

				Arc::new (
					backup_instruction.get_bytes_to_emit ().to_vec ())

				)).boxed ()
			)).boxed ()

		} else {

			futures::failed::<BoxFuture <ChunkData, String>, String> (
				"Instruction with neither chunk or bytes".to_string ()
			).boxed ()

		}

	}

	#[ doc (hidden) ]
	pub fn follow_instructions (
		& self,
		input: & mut Read,
		target: & mut Write,
		digest: & mut Digest,
		progress: & Fn (u64),
	) -> Result <(), String> {

		let mut coded_input_stream =
			CodedInputStream::new (
				input);

		let mut count: u64 = 0;

		enum JobTarget {
			Chunk (ChunkData),
			FutureChunk (BoxFuture <ChunkData, String>),
		}

		type Job = BoxFuture <JobTarget, String>;

		let mut current_chunk_job: Option <Job> =
			None;

		let mut next_chunk_jobs: LinkedList <Job> =
			LinkedList::new ();

		let mut future_chunk_job: Option <Job> =
			None;

		let mut eof = false;

		loop {

			// load next instruction, if we have room

			if future_chunk_job.is_none () && ! eof {

				if (
					try! (
						protobuf_result (
							coded_input_stream.eof ()))
				) {

					eof = true;

				} else {

					let backup_instruction: proto::BackupInstruction =
						read_message (
							& mut coded_input_stream,
							|| format! (
								"backup instruction"),
						) ?;

					future_chunk_job = Some (

						self.follow_instruction_async_async (
							& backup_instruction,
						).map (
							|future_chunk_data|

							JobTarget::FutureChunk (
								future_chunk_data)

						).boxed ()

					);

				}

			}

			// wait for something to happen

			if current_chunk_job.is_none () {

				current_chunk_job =
					next_chunk_jobs.pop_front ();

			}

			let have_current_chunk_job =
				current_chunk_job.is_some ();

			let have_future_chunk_job =
				future_chunk_job.is_some ();

			if (
				! have_current_chunk_job
				&& ! have_future_chunk_job
			) {
				break;
			}

			let completed_job_target =
				match futures::select_all (vec! [

				current_chunk_job.unwrap_or_else (
					|| futures::empty ().boxed ()),

				future_chunk_job.unwrap_or_else (
					|| futures::empty ().boxed ()),

			]).wait () {

				Ok ((value, 0, remaining_future)) => {

					future_chunk_job =
						if have_future_chunk_job {

							Some (
								remaining_future.into_iter ()
									.next ()
									.unwrap ()
									.boxed ()
							)

						} else { None };

					current_chunk_job = None;

					value

				},

				Ok ((value, 1, remaining_future)) => {

					current_chunk_job =
						if have_current_chunk_job {

							Some (
								remaining_future.into_iter ()
									.next ()
									.unwrap ()
									.boxed ()
							)

						} else { None };

					future_chunk_job = None;

					value

				},

				Ok ((_, _, _)) =>
					panic! ("Not possible"),

				Err ((error, _, _)) =>
					return Err (error),

			};

			// handle the something that happened

			match completed_job_target {

				JobTarget::Chunk (chunk_data) => {

					digest.input (
						& chunk_data);

					io_result (
						target.write (
							& chunk_data)
					) ?;

					progress (
						count);

					count += 1;

				},

				JobTarget::FutureChunk (future_chunk) => {

					next_chunk_jobs.push_back (

						future_chunk.map (
							|chunk_data|

							JobTarget::Chunk (
								chunk_data)

						).boxed ()

					);

				},

			};

		}

		Ok (())

	}

	/// This will load a single chunk from the repository. It can be used to
	/// create advanced behaviours, and is used, for example, by the
	/// `RandomAccess` struct.

	pub fn get_chunk (
		& self,
		chunk_id: ChunkId,
	) -> Result <ChunkData, String> {

		self.get_chunk_async (
			chunk_id,
		).wait ()

	}

	/// This will load a single chunk from the repository, returning immediately
	/// with a future which can later be waited for. The chunk will be loaded in
	/// the background using the cpu pool.

	pub fn get_chunk_async (
		& self,
		chunk_id: ChunkId,
	) -> BoxFuture <ChunkData, String> {

		self.get_chunk_async_async (
			chunk_id,
		).and_then (
			|future|

			future.wait ()

		).boxed ()

	}

	/// This will load a single chunk from the repository, returning immediately
	/// with a future which will complete immediately if the chunk is in cache,
	/// with a future which will complete immediately with the chunk data.
	///
	/// If the chunk is not in cache, the returned future will wait until there
	/// is an available thread to start loading the bundle containing the
	/// chunk's data. It will then complete with a future which will in turn
	/// complete when the bundle has been loaded.
	///
	/// This double-asynchronicity allows consumers to efficiently use all
	/// available threads while blocking when none are available. This should
	/// significantly reduce worst-case memory usage.

	pub fn get_chunk_async_async (
		& self,
		chunk_id: ChunkId,
	) -> BoxFuture <BoxFuture <ChunkData, String>, String> {

		let mut self_state =
			self.state.lock ().unwrap ();

		if self_state.master_index.is_none () {

			panic! (
				"Must load indexes before getting chunks");

		}

		// lookup via storage manager

		if let Some (chunk_data_future) =
			self.storage_manager.get (
				& chunk_id.to_hex (),
			) {

			let self_clone =
				self.clone ();

			return futures::done (
				Ok (chunk_data_future),
			).or_else (
				move |_error: String| {

				let mut self_state =
					self_clone.state.lock ().unwrap ();

				self_clone.load_chunk_async_async (
					self_state.deref_mut (),
					chunk_id)

			}).boxed ();

		}

		// load bundle if chunk is not available

		self.load_chunk_async_async (
			self_state.deref_mut (),
			chunk_id)

	}

	fn load_chunk_async_async (
		& self,
		self_state: & mut RepositoryState,
		chunk_id: ChunkId,
	) -> BoxFuture <BoxFuture <ChunkData, String>, String> {

		// get bundle id

		let bundle_id = match (

			self_state.master_index.as_ref ().unwrap ().get (
				& chunk_id,
			).clone ()

		) {

			Some (index_entry) =>
				index_entry.bundle_id,

			None => {

				return futures::failed (
					format! (
						"Missing chunk: {}",
						chunk_id.to_hex ()),
				).boxed ();

			},

		};

		self.load_chunk_async_async_real (
			self_state,
			chunk_id,
			bundle_id)

	}

	fn load_chunk_async_async_real (
		& self,
		self_state: & mut RepositoryState,
		chunk_id: ChunkId,
		bundle_id: BundleId,
	) -> BoxFuture <BoxFuture <ChunkData, String>, String> {

		let self_clone =
			self.clone ();

		// if it's already being loaded then we can join in

		if self_state.bundles_loading.contains_key (
			& bundle_id) {

			return futures::done (
				Ok (

				self_clone.join_load_chunk_async (
					& mut self_state.bundles_loading.get_mut (
						& bundle_id,
					).unwrap (),
					chunk_id.clone ())

				)

			).boxed ();

		}

		// start a load if there is a slot

		if self_state.bundles_loading.len ()
			< self.data.config.max_threads {

			return futures::done (Ok (

				self_clone.start_load_chunk_async (
					self_state,
					chunk_id.clone (),
					bundle_id,
				)

			)).boxed ();

		}

		// add to future bundle loaders

		if ! self_state.bundles_to_load.contains_key (
			& bundle_id) {

			self_state.bundles_to_load.insert (
				bundle_id.clone (),
				HashMap::new ());

			self_state.bundles_to_load_list.push_back (
				bundle_id.clone ());

		}

		self.join_future_load_chunk_async (
			self_state.bundles_to_load.get_mut (
				& bundle_id,
			).unwrap (),
			chunk_id)

	}

	fn join_load_chunk_async (
		& self,
		bundle_waiters: & mut BundleWaiters,
		chunk_id: ChunkId,
	) -> BoxFuture <ChunkData, String> {

		let (complete, future) =
			futures::oneshot ();

		if (

			! bundle_waiters.contains_key (
				& chunk_id)

		) {

			bundle_waiters.insert (
				chunk_id.clone (),
				Vec::new ());

		}

		bundle_waiters.get_mut (
			& chunk_id,
		).unwrap ().push (
			complete,
		);

		future.map_err (
			|_|

			"Cancelled".to_owned ()

		).and_then (
			|chunk_data_result| {

			chunk_data_result

		}).boxed ()

	}

	fn join_future_load_chunk_async (
		& self,
		bundle_waiters: & mut FutureBundleWaiters,
		chunk_id: ChunkId,
	) -> BoxFuture <BoxFuture <ChunkData, String>, String> {

		// insert chunk id if it does not already exist

		if (

			! bundle_waiters.contains_key (
				& chunk_id)

		) {

			bundle_waiters.insert (
				chunk_id.clone (),
				Vec::new ());

		}

		// add oneshot to list

		let (complete, future) =
			futures::oneshot ();

		bundle_waiters.get_mut (
			& chunk_id,
		).unwrap ().push (
			complete,
		);

		// return appropriately typed future

		future.and_then (
			|next_future|

			Ok (next_future)

		).map_err (
			|_|

			"Cancelled".to_string ()

		).boxed ()

	}

	fn start_load_chunk_async (
		& self,
		self_state: & mut RepositoryState,
		chunk_id: ChunkId,
		bundle_id: BundleId,
	) -> BoxFuture <ChunkData, String> {

		let bundle_path =
			self.bundle_path (
				bundle_id);

		self_state.bundles_loading.insert (
			bundle_id.clone (),
			HashMap::new ());

		let mut self_clone =
			self.clone ();

		self.cpu_pool.spawn_fn (
			move || {

			let chunk_map_result = (

				read_bundle (
					bundle_path,
					self_clone.data.encryption_key)

			).map_err (
				|original_error| {

				format! (
					"Error reading bundle {}: {}",
					bundle_id.to_hex (),
					original_error)

			}).map (
				move |bundle_data| {

				let mut chunk_map =
					HashMap::new ();

				for (found_chunk_id, found_chunk_data) in bundle_data {

					chunk_map.insert (
						found_chunk_id,
						Arc::new (
							found_chunk_data));

				}

				Arc::new (chunk_map)

			});

			// store chunk data in cache

			let mut self_state =
				self_clone.state.lock ().unwrap ();

			let chunk_map =
				chunk_map_result ?;

			for (chunk_id, chunk_data)
			in chunk_map.iter () {

				try! (
					self_clone.storage_manager.insert (
						chunk_id.to_hex (),
						chunk_data.clone ()));

			}

			// notify other processes waiting for the same bundle

			let bundle_waiters =
				self_state.bundles_loading.remove (
					& bundle_id,
				).unwrap ();

			for (chunk_id, chunk_waiters)
			in bundle_waiters {

				let chunk_data_result = (

					chunk_map.get (
						& chunk_id,
					).ok_or_else (
						||

						format! (
							"Expected to find chunk {} in bundle {}",
							chunk_id.to_hex (),
							bundle_id.to_hex ())

					)

				);

				for chunk_waiter in chunk_waiters {

					chunk_waiter.complete (
						chunk_data_result.clone (
						).map (
							|chunk_data|
							chunk_data.clone ()
						),
					);

				}

			}

			// start loading next chunks

			self_clone.start_loading_next_chunks (
				self_state.deref_mut ());

			// return

			chunk_map.get (
				& chunk_id,
			).ok_or_else (
				||

				format! (
					"Expected to find chunk {} in bundle {}",
					chunk_id.to_hex (),
					bundle_id.to_hex ())

			).map (
				|chunk_data|
				chunk_data.clone ()
			)

		}).boxed ()

	}

	fn start_loading_next_chunks (
		& self,
		self_state: & mut RepositoryState,
	) {

		let bundle_id = match (
			self_state.bundles_to_load_list.pop_front ()
		) {

			Some (bundle_id) =>
				bundle_id,

			None =>
				return,

		};

		let mut bundle_waiters =
			self_state.bundles_to_load.remove (
				& bundle_id,
			).unwrap ();

		// first waiter of first chunk starts things off

		let first_chunk_id =
			bundle_waiters.keys ().next ().unwrap ().clone ();

		let first_waiters =
			bundle_waiters.remove (
				& first_chunk_id,
			).unwrap ();

		let mut first_waiters_iterator =
			first_waiters.into_iter ();

		first_waiters_iterator.next ().unwrap ().complete (

			self.start_load_chunk_async (
				self_state,
				first_chunk_id.clone (),
				bundle_id,
			)

		);

		// the rest join in

		let new_bundle_waiters =
			self_state.bundles_loading.get_mut (
				& bundle_id,
			).unwrap ();

		for first_waiter in first_waiters_iterator {

			first_waiter.complete (

				self.join_load_chunk_async (
					new_bundle_waiters,
					first_chunk_id.clone (),
				)

			);

		}

		for (chunk_id, waiters) in bundle_waiters {

			for waiter in waiters {

				waiter.complete (

					self.join_load_chunk_async (
						new_bundle_waiters,
						chunk_id.clone (),
					)

				);

			}

		}

	}

	/// This will load a single index entry from the repository. It returns this
	/// as a `MasterIndexEntry`, which includes the index entry and the header
	/// from the index file, since both are generally needed to do anything
	/// useful.
	///
	/// It can be used to create advanced behaviours, and is used, for example,
	/// by the `RandomAccess` struct.

	pub fn get_index_entry (
		& self,
		chunk_id: ChunkId,
	) -> Result <MasterIndexEntry, String> {

		let self_state =
			self.state.lock ().unwrap ();

		if self_state.master_index.is_none () {

			panic! (
				"Must load indexes before getting index entries");

		}

		match (

			self_state.master_index.as_ref ().unwrap ().get (
				& chunk_id,
			).clone ()

		) {

			Some (value) =>
				Ok (value.clone ()),

			None =>
				Err (
					format! (
						"Missing chunk: {}",
						chunk_id.to_hex ())
				),

		}

	}

	/// Returns true if a chunk is present in the loaded indexes

	pub fn has_chunk (
		& self,
		chunk_id: ChunkId,
	) -> bool {

		let self_state =
			self.state.lock ().unwrap ();

		if self_state.master_index.is_none () {

			panic! (
				"Must load indexes before getting index entries");

		}

		self_state.master_index.as_ref ().unwrap ().get (
			& chunk_id,
		).is_some ()

	}

	/// This is a convenience method to construct a `RandomAccess` struct. It
	/// simply calls the `RandomAccess::new` constructor.

	pub fn open_backup (
		& self,
		output: & Output,
		backup_name: & str,
	) -> Result <RandomAccess, String> {

		RandomAccess::new (
			output,
			self,
			backup_name)

	}

	/// This is an accessor method to access the `RepositoryConfig` struct which
	/// was used to construct this `Repository`.

	pub fn config (
		& self,
	) -> & RepositoryConfig {
		& self.data.config
	}

	pub fn path (
		& self,
	) -> & Path {
		& self.data.path
	}

	/// This is an accessor method to access the `StorageInfo` protobug struct
	/// which was loaded from the repository's index file.

	pub fn storage_info (
		& self,
	) -> & proto::StorageInfo {
		& self.data.storage_info
	}

	/// This is an accessor method to access the decrypted encryption key which
	/// was stored in the repository's info file and decrypted using the
	/// provided password.

	pub fn encryption_key (
		& self,
	) -> Option <[u8; KEY_SIZE]> {
		self.data.encryption_key
	}

	/// Convenience function to return the filesystem path for an index id.

	pub fn index_path (
		& self,
		index_id: IndexId,
	) -> PathBuf {

		self.data.path
			.join ("index")
			.join (index_id.to_hex ())

	}

	/// Convenience function to return the filesystem path for a bundle id.

	pub fn bundle_path (
		& self,
		bundle_id: BundleId,
	) -> PathBuf {

		self.data.path
			.join ("bundles")
			.join (bundle_id [0 .. 1].to_hex ())
			.join (bundle_id.to_hex ())

	}

}
