use libc::c_int;
use libc::size_t;

use std::io;
use std::io::BufRead;
use std::io::Read;
use std::ptr;

#[ repr (C) ]
struct LzmaStream {

	next_in: * const u8,
	avail_in: size_t,
	total_in: u64,

	next_out: * mut u8,
	avail_out: size_t,
	total_out: u64,

	allocator: * const u8,
	internal: * const u8,

	reserved_pointer_1: * const u8,
	reserved_pointer_2: * const u8,
	reserved_pointer_3: * const u8,
	reserved_pointer_4: * const u8,

	reserved_int_1: u64,
	reserved_int_2: u64,

	reserved_int_3: size_t,
	reserved_int_4: size_t,

	reserved_enum_1: u32,
	reserved_enum_2: u32,

}

// return values

const LZMA_OK: c_int = 0;
const LZMA_STREAM_END: c_int = 1;
//const LZMA_NO_CHECK: c_int = 2;
//const LZMA_UNSUPPORTED_CHECK: c_int = 3;
//const LZMA_GET_CHECK: c_int = 4;
//const LZMA_MEM_ERROR: c_int = 5;
//const LZMA_MEMLIMIT_ERROR: c_int = 6;
//const LZMA_FORMAT_ERROR: c_int = 7;
//const LZMA_OPTIONS_ERROR: c_int = 8;
//const LZMA_DATA_ERROR: c_int = 9;
//const LZMA_BUF_ERROR: c_int = 10;
//const LZMA_PROG_ERROR: c_int = 11;

// action values

const LZMA_RUN: c_int = 0;

#[ link (name = "lzma") ]
extern {

	fn lzma_code (
		strm: * mut LzmaStream,
		action: c_int,
	) -> c_int;

	fn lzma_stream_decoder (
		strm: * mut LzmaStream,
		memlimit: u64,
		flags: u32,
	) -> c_int;

	fn lzma_end (
		strm: * mut LzmaStream,
	);

}

pub struct LzmaReader <'a> {
	input: & 'a mut BufRead,
	lzma_stream: LzmaStream,
	error: bool,
	eof: bool,
}

impl <'a> LzmaReader <'a> {

	pub fn new (
		input: & 'a mut BufRead,
	) -> Result <LzmaReader <'a>, String> {

		let mut lzma_stream = LzmaStream {

			next_in: ptr::null (),
			avail_in: 0,
			total_in: 0,

			next_out: ptr::null_mut (),
			avail_out: 0,
			total_out: 0,

			allocator: ptr::null (),
			internal: ptr::null (),

			reserved_pointer_1: ptr::null (),
			reserved_pointer_2: ptr::null (),
			reserved_pointer_3: ptr::null (),
			reserved_pointer_4: ptr::null (),

			reserved_int_1: 0,
			reserved_int_2: 0,

			reserved_int_3: 0,
			reserved_int_4: 0,

			reserved_enum_1: 0,
			reserved_enum_2: 0,

		};

		let init_result = unsafe {
			lzma_stream_decoder (
				& mut lzma_stream,
				u64::max_value (),
				0,
			)
		};

		if init_result != LZMA_OK {

			return Err (
				format! (
					"Error initialising lzma decoder: {}",
					init_result));

		}

		Ok (LzmaReader {
			input: input,
			lzma_stream: lzma_stream,
			error: false,
			eof: false,
		})

	}

}

impl <'a> Read for LzmaReader <'a> {

	fn read (
		& mut self,
		output_buffer: & mut [u8],
	) -> io::Result <usize> {

		if self.error {
			panic! (
				"Error already");
		}

		if self.eof {
			return Ok (0);
		}

		// set output buffer

		self.lzma_stream.next_out =
			& mut output_buffer [0];

		self.lzma_stream.avail_out =
			output_buffer.len ();

		loop {

			// read input

			let prev_total_in =
				self.lzma_stream.total_in;

			let decode_result;

			{

				let input_buffer =
					try! (
						self.input.fill_buf ());

				if input_buffer.len () == 0 {

					self.error = true;

					return Err (
						io::Error::new (
							io::ErrorKind::InvalidData,
							"LZMA stream truncated"));

				}

				self.lzma_stream.next_in =
					& input_buffer [0];

				self.lzma_stream.avail_in =
					input_buffer.len ();

				// perform decompression

				decode_result = unsafe {
					lzma_code (
						& mut self.lzma_stream,
						LZMA_RUN,
					)
				};

			}

			self.input.consume (
				self.lzma_stream.total_in as usize
					- prev_total_in as usize);

			// handle stream end

			if decode_result == LZMA_STREAM_END {

				self.eof = true;

				return Ok (
					output_buffer.len () as usize
						- self.lzma_stream.avail_out as usize
				);

			}

			// handle error

			if decode_result != LZMA_OK {

				self.error = true;

				return Err (
					io::Error::new (
						io::ErrorKind::InvalidData,
						format! (
							"LZMA error: {}",
							decode_result)));

			}

			// handle output buffer full

			if self.lzma_stream.avail_out == 0 {

				return Ok (
					output_buffer.len ());

			}

		}

	}

}

impl <'a> Drop for LzmaReader <'a> {

	fn drop (
		& mut self,
	) {

		unsafe {
			lzma_end (
				& mut self.lzma_stream,
			);
		}

	}

}

// ex: noet ts=4 filetype=rust
