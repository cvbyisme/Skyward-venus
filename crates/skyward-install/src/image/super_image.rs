//! Raw and Android sparse images of the physical `super` partition.
//!
//! Sparse images are read as a seekable expanded stream, so the existing
//! partition writer can write and read back the complete physical image
//! without ever allocating the expanded image in memory.

use anyhow::{Context, Result, bail, ensure};
use crc32fast::Hasher;
use skyward_lp::Metadata;
use std::io::{self, Read, Seek, SeekFrom};

const SPARSE_MAGIC: u32 = 0xed26_ff3a;
const SPARSE_MAGIC_BYTES: [u8; 4] = SPARSE_MAGIC.to_le_bytes();
const LP_GEOMETRY_MAGIC: u32 = 0x616c_4467;
const LP_GEOMETRY_MAGIC_BYTES: [u8; 4] = LP_GEOMETRY_MAGIC.to_le_bytes();
const SPARSE_HEADER_SIZE: usize = 28;
const SPARSE_CHUNK_HEADER_SIZE: usize = 12;
const CHUNK_RAW: u16 = 0xcac1;
const CHUNK_FILL: u16 = 0xcac2;
const CHUNK_DONT_CARE: u16 = 0xcac3;
const CHUNK_CRC32: u16 = 0xcac4;
const MAX_CHUNKS: u32 = 1_000_000;
const MAX_METADATA_PREFIX: usize = 16 << 20;

#[derive(Debug, Clone, Copy)]
enum ChunkData {
    Raw { offset: u64 },
    Fill([u8; 4]),
    Zero,
    Crc32(u32),
}

#[derive(Debug)]
struct Chunk {
    output_start: u64,
    output_len: u64,
    data: ChunkData,
}

struct SparseImage<R> {
    input: R,
    chunks: Vec<Chunk>,
    expanded_size: u64,
    position: u64,
    image_checksum: u32,
}

/// A complete super image. Its `Read` and `Seek` view is the expanded image,
/// whether the source on disk is raw or Android sparse.
pub struct SuperImage<R> {
    input: Contents<R>,
    expanded_size: u64,
    source_start: u64,
}

enum Contents<R> {
    Raw(R),
    Sparse(SparseImage<R>),
}

/// Whether the first bytes or the LP geometry at offset 4096 identify a
/// complete physical super image. Sparse magic alone is not enough: sparse
/// logical-partition images use the same container format, so the LP metadata
/// for both slots must parse before this is recognized as super.
pub fn is_super<R: Read + Seek>(file: &mut R, head: &[u8]) -> Result<bool> {
    let sparse = head.starts_with(&SPARSE_MAGIC_BYTES);
    let raw = if sparse {
        true
    } else {
        let position = file.stream_position().context("remember image position")?;
        let result = (|| -> Result<bool> {
            file.seek(SeekFrom::Start(4096))
                .context("seek to super geometry")?;
            let mut magic = [0u8; 4];
            match file.read_exact(&mut magic) {
                Ok(()) => Ok(magic == LP_GEOMETRY_MAGIC_BYTES),
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
                Err(e) => Err(e).context("read super geometry magic"),
            }
        })();
        file.seek(SeekFrom::Start(position))
            .context("restore image position")?;
        result?
    };
    if !raw {
        return Ok(false);
    }

    let position = file.stream_position().context("remember image position")?;
    let result = (|| -> Result<bool> {
        file.seek(SeekFrom::Start(0))
            .context("rewind candidate super image")?;
        let mut image = match SuperImage::open(&mut *file) {
            Ok(image) => image,
            Err(_) => return Ok(false),
        };
        if image.expanded_size() < 12288 {
            return Ok(false);
        }
        let mut geometry = [0u8; 12288];
        image
            .read_exact(&mut geometry)
            .context("read candidate super geometry")?;
        let needed = match skyward_lp::metadata_prefix_size(&geometry) {
            Ok(needed) if needed <= MAX_METADATA_PREFIX => needed,
            _ => return Ok(false),
        };
        let mut prefix = vec![0u8; needed];
        image
            .seek(SeekFrom::Start(0))
            .context("rewind candidate super metadata")?;
        image
            .read_exact(&mut prefix)
            .context("read candidate super metadata")?;
        let a = match skyward_lp::read_bytes(&prefix, 0) {
            Ok(metadata) => metadata,
            Err(_) => return Ok(false),
        };
        let b = match skyward_lp::read_bytes(&prefix, 1) {
            Ok(metadata) => metadata,
            Err(_) => return Ok(false),
        };
        Ok(a.damaged.is_empty()
            && b.damaged.is_empty()
            && a.block_devices.len() == 1
            && b.block_devices.len() == 1
            && a.block_devices[0].partition_name == "super"
            && b.block_devices[0].partition_name == "super"
            && a.partition("system_a").is_some()
            && b.partition("system_b").is_some())
    })();
    file.seek(SeekFrom::Start(position))
        .context("restore image position")?;
    result
}

impl<R: Read + Seek> SuperImage<R> {
    pub fn open(mut input: R) -> Result<Self> {
        let start = input
            .stream_position()
            .context("remember super image start")?;
        let end = input
            .seek(SeekFrom::End(0))
            .context("find super image length")?;
        ensure!(end >= start, "super image has an invalid length");
        let input_len = end - start;
        input
            .seek(SeekFrom::Start(start))
            .context("rewind super image")?;
        let mut magic = [0u8; 4];
        input
            .read_exact(&mut magic)
            .context("read super image magic")?;
        input
            .seek(SeekFrom::Start(start))
            .context("rewind super image")?;

        if magic == SPARSE_MAGIC_BYTES {
            let sparse = SparseImage::parse(input, start, end)?;
            let expanded_size = sparse.expanded_size;
            Ok(Self {
                input: Contents::Sparse(sparse),
                expanded_size,
                source_start: start,
            })
        } else {
            ensure!(
                input_len >= 8192,
                "raw super image is shorter than its LP geometry"
            );
            input
                .seek(SeekFrom::Start(start + 4096))
                .context("seek to super geometry")?;
            let mut geometry = [0u8; 4];
            input
                .read_exact(&mut geometry)
                .context("read super geometry")?;
            ensure!(
                geometry == LP_GEOMETRY_MAGIC_BYTES,
                "image is neither a raw super image nor an Android sparse image"
            );
            input
                .seek(SeekFrom::Start(start))
                .context("rewind raw super image")?;
            Ok(Self {
                input: Contents::Raw(input),
                expanded_size: input_len,
                source_start: start,
            })
        }
    }

    pub fn expanded_size(&self) -> u64 {
        self.expanded_size
    }

    /// Validate both A/B LP metadata copies before the image can be offered
    /// for flashing. The physical image must exactly fill this device's
    /// super partition, and all metadata must be primary-copy clean.
    pub fn validate(&mut self, partition_size: u64) -> Result<[Metadata; 2]> {
        ensure!(
            self.expanded_size == partition_size,
            "expanded super image is {} bytes, but this device's super partition is {partition_size} bytes",
            self.expanded_size
        );

        let mut geometry_prefix = vec![0u8; 12288];
        self.seek(SeekFrom::Start(0))
            .context("rewind super image for metadata")?;
        self.read_exact(&mut geometry_prefix)
            .context("read super geometry copies")?;
        let needed = skyward_lp::metadata_prefix_size(&geometry_prefix)?;
        ensure!(
            needed <= MAX_METADATA_PREFIX,
            "super LP metadata region is {needed} bytes, over the {MAX_METADATA_PREFIX}-byte safety limit"
        );
        ensure!(
            needed as u64 <= self.expanded_size,
            "super image ends inside its LP metadata region"
        );

        let mut prefix = vec![0u8; needed];
        self.seek(SeekFrom::Start(0))
            .context("rewind super image metadata")?;
        self.read_exact(&mut prefix)
            .context("read super LP metadata region")?;
        let a = skyward_lp::read_bytes(&prefix, 0).context("validate super metadata for slot A")?;
        let b = skyward_lp::read_bytes(&prefix, 1).context("validate super metadata for slot B")?;
        for (slot, metadata) in [("A", &a), ("B", &b)] {
            ensure!(
                metadata.damaged.is_empty(),
                "super metadata for slot {slot} has damaged primary copies: {}",
                metadata.damaged.join("; ")
            );
            ensure!(
                metadata.geometry.metadata_slot_count >= 2,
                "super metadata has fewer than two A/B metadata slots"
            );
            ensure!(
                metadata.block_devices.len() == 1,
                "super image names {} block devices; this recovery only writes a single physical super partition",
                metadata.block_devices.len()
            );
            let device = &metadata.block_devices[0];
            ensure!(
                device.partition_name == "super",
                "super image's physical block device is {:?}, not super",
                device.partition_name
            );
            ensure!(
                device.size == partition_size,
                "super metadata describes {} bytes, but the physical partition is {partition_size} bytes",
                device.size
            );
            ensure!(
                metadata
                    .partition(&format!(
                        "system_{slot_lower}",
                        slot_lower = slot.to_ascii_lowercase()
                    ))
                    .is_some(),
                "super metadata for slot {slot} has no system partition"
            );
        }
        self.seek(SeekFrom::Start(0))
            .context("rewind validated super image")?;
        Ok([a, b])
    }
}

impl<R: Read + Seek> Read for SuperImage<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        match &mut self.input {
            Contents::Raw(input) => input.read(out),
            Contents::Sparse(input) => input.read(out),
        }
    }
}

impl<R: Read + Seek> Seek for SuperImage<R> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        match &mut self.input {
            Contents::Raw(input) => {
                let absolute = match from {
                    SeekFrom::Start(offset) => {
                        let offset = self.source_start.checked_add(offset).ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "super image seek overflows",
                            )
                        })?;
                        input.seek(SeekFrom::Start(offset))?
                    }
                    SeekFrom::Current(offset) => input.seek(SeekFrom::Current(offset))?,
                    SeekFrom::End(offset) => input.seek(SeekFrom::End(offset))?,
                };
                absolute.checked_sub(self.source_start).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "super image seek precedes its start",
                    )
                })
            }
            Contents::Sparse(input) => input.seek(from),
        }
    }
}

impl<R: Read + Seek> SparseImage<R> {
    fn parse(mut input: R, start: u64, end: u64) -> Result<Self> {
        input
            .seek(SeekFrom::Start(start))
            .context("seek to sparse header")?;
        let mut header = [0u8; SPARSE_HEADER_SIZE];
        input
            .read_exact(&mut header)
            .context("read Android sparse header")?;
        ensure!(
            u32::from_le_bytes(header[0..4].try_into().unwrap()) == SPARSE_MAGIC,
            "bad Android sparse magic"
        );
        let major = u16::from_le_bytes(header[4..6].try_into().unwrap());
        let minor = u16::from_le_bytes(header[6..8].try_into().unwrap());
        let file_header_size = u16::from_le_bytes(header[8..10].try_into().unwrap()) as u64;
        let chunk_header_size = u16::from_le_bytes(header[10..12].try_into().unwrap()) as u64;
        let block_size = u32::from_le_bytes(header[12..16].try_into().unwrap()) as u64;
        let total_blocks = u32::from_le_bytes(header[16..20].try_into().unwrap()) as u64;
        let total_chunks = u32::from_le_bytes(header[20..24].try_into().unwrap());
        let image_checksum = u32::from_le_bytes(header[24..28].try_into().unwrap());

        ensure!(
            major == 1 && minor == 0,
            "unsupported Android sparse version {major}.{minor}"
        );
        ensure!(
            file_header_size >= SPARSE_HEADER_SIZE as u64,
            "sparse file header is too short"
        );
        ensure!(
            chunk_header_size >= SPARSE_CHUNK_HEADER_SIZE as u64,
            "sparse chunk header is too short"
        );
        ensure!(
            block_size >= 512 && block_size.is_multiple_of(4),
            "invalid sparse block size {block_size}"
        );
        ensure!(total_blocks != 0, "sparse image has no output blocks");
        ensure!(
            total_chunks != 0 && total_chunks <= MAX_CHUNKS,
            "invalid sparse chunk count {total_chunks}"
        );
        let expanded_size = total_blocks
            .checked_mul(block_size)
            .context("sparse output size overflows")?;
        let header_end = start
            .checked_add(file_header_size)
            .context("sparse header offset overflows")?;
        ensure!(header_end <= end, "sparse file header runs past the input");
        input
            .seek(SeekFrom::Start(header_end))
            .context("skip sparse file header extensions")?;

        let mut chunks = Vec::with_capacity(total_chunks as usize);
        let mut output_at = 0u64;
        for index in 0..total_chunks {
            let chunk_start = input
                .stream_position()
                .context("read sparse chunk position")?;
            let mut raw = [0u8; SPARSE_CHUNK_HEADER_SIZE];
            input
                .read_exact(&mut raw)
                .with_context(|| format!("read sparse chunk {index}"))?;
            let kind = u16::from_le_bytes(raw[0..2].try_into().unwrap());
            let reserved = u16::from_le_bytes(raw[2..4].try_into().unwrap());
            let blocks = u32::from_le_bytes(raw[4..8].try_into().unwrap()) as u64;
            let total_size = u32::from_le_bytes(raw[8..12].try_into().unwrap()) as u64;
            ensure!(
                reserved == 0,
                "sparse chunk {index} has nonzero reserved bits"
            );
            let data_offset = chunk_start
                .checked_add(chunk_header_size)
                .context("sparse chunk data offset overflows")?;
            let output_len = blocks
                .checked_mul(block_size)
                .context("sparse chunk output size overflows")?;
            let data_len = match kind {
                CHUNK_RAW => output_len,
                CHUNK_FILL | CHUNK_CRC32 => 4,
                CHUNK_DONT_CARE => 0,
                other => bail!("unsupported Android sparse chunk type {other:#06x}"),
            };
            ensure!(
                total_size
                    == chunk_header_size
                        .checked_add(data_len)
                        .context("sparse chunk size overflows")?,
                "sparse chunk {index} has total size {total_size}, expected {}",
                chunk_header_size + data_len
            );
            let data_end = data_offset
                .checked_add(data_len)
                .context("sparse chunk end overflows")?;
            ensure!(data_end <= end, "sparse chunk {index} runs past the input");

            // The sparse format permits future chunk-header extensions.
            // Skip them before reading FILL/CRC payloads; RAW reads already
            // use the recorded absolute data offset.
            input
                .seek(SeekFrom::Start(data_offset))
                .context("skip sparse chunk header extensions")?;
            let data = match kind {
                CHUNK_RAW => {
                    ensure!(blocks != 0, "sparse raw chunk {index} is empty");
                    ChunkData::Raw {
                        offset: data_offset,
                    }
                }
                CHUNK_FILL => {
                    ensure!(blocks != 0, "sparse fill chunk {index} is empty");
                    let mut pattern = [0u8; 4];
                    input
                        .read_exact(&mut pattern)
                        .context("read sparse fill pattern")?;
                    ChunkData::Fill(pattern)
                }
                CHUNK_DONT_CARE => {
                    ensure!(blocks != 0, "sparse don't-care chunk {index} is empty");
                    ChunkData::Zero
                }
                CHUNK_CRC32 => {
                    ensure!(blocks == 0, "sparse CRC chunk {index} has output blocks");
                    let mut crc = [0u8; 4];
                    input.read_exact(&mut crc).context("read sparse CRC32")?;
                    ChunkData::Crc32(u32::from_le_bytes(crc))
                }
                _ => unreachable!(),
            };
            chunks.push(Chunk {
                output_start: output_at,
                output_len,
                data,
            });
            output_at = output_at
                .checked_add(output_len)
                .context("sparse output offset overflows")?;
            input
                .seek(SeekFrom::Start(data_end))
                .context("skip sparse chunk data")?;
        }
        ensure!(
            output_at == expanded_size,
            "sparse chunks describe {output_at} bytes, header says {expanded_size}"
        );
        ensure!(
            input.stream_position().context("finish sparse image")? == end,
            "sparse image has trailing or unparsed bytes"
        );

        let mut image = Self {
            input,
            chunks,
            expanded_size,
            position: 0,
            image_checksum,
        };
        image.verify_checksums()?;
        image
            .input
            .seek(SeekFrom::Start(start))
            .context("rewind sparse source")?;
        Ok(image)
    }

    fn read_chunk(input: &mut R, chunk: &Chunk, offset: u64, out: &mut [u8]) -> io::Result<()> {
        match chunk.data {
            ChunkData::Raw { offset: data } => {
                input.seek(SeekFrom::Start(data + offset))?;
                input.read_exact(out)
            }
            ChunkData::Fill(pattern) => {
                for (i, byte) in out.iter_mut().enumerate() {
                    *byte = pattern[((offset + i as u64) % 4) as usize];
                }
                Ok(())
            }
            ChunkData::Zero => {
                out.fill(0);
                Ok(())
            }
            ChunkData::Crc32(_) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "CRC chunk has no output bytes",
            )),
        }
    }

    fn verify_checksums(&mut self) -> Result<()> {
        let has_chunk_crc = self
            .chunks
            .iter()
            .any(|c| matches!(c.data, ChunkData::Crc32(_)));
        if self.image_checksum == 0 && !has_chunk_crc {
            return Ok(());
        }
        let mut checksum = Hasher::new();
        let mut buffer = vec![0u8; super::IO_CHUNK];
        for index in 0..self.chunks.len() {
            let chunk_len = self.chunks[index].output_len;
            if let ChunkData::Crc32(expected) = self.chunks[index].data {
                ensure!(
                    checksum.clone().finalize() == expected,
                    "sparse CRC32 chunk {index} does not match expanded data"
                );
                continue;
            }
            let mut offset = 0u64;
            while offset < chunk_len {
                let n = (chunk_len - offset).min(buffer.len() as u64) as usize;
                Self::read_chunk(
                    &mut self.input,
                    &self.chunks[index],
                    offset,
                    &mut buffer[..n],
                )
                .with_context(|| format!("read sparse chunk {index} for checksum"))?;
                checksum.update(&buffer[..n]);
                offset += n as u64;
            }
        }
        if self.image_checksum != 0 {
            ensure!(
                checksum.finalize() == self.image_checksum,
                "Android sparse image checksum does not match expanded data"
            );
        }
        Ok(())
    }
}

impl<R: Read + Seek> Read for SparseImage<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() || self.position >= self.expanded_size {
            return Ok(0);
        }
        let mut written = 0usize;
        while written < out.len() && self.position < self.expanded_size {
            let index = self
                .chunks
                .partition_point(|c| c.output_start + c.output_len <= self.position);
            let Some(chunk) = self.chunks.get(index) else {
                break;
            };
            if chunk.output_len == 0 {
                continue;
            }
            let inside = self.position - chunk.output_start;
            let n = (chunk.output_len - inside).min((out.len() - written) as u64) as usize;
            Self::read_chunk(
                &mut self.input,
                chunk,
                inside,
                &mut out[written..written + n],
            )?;
            self.position += n as u64;
            written += n;
        }
        if written == 0 && self.position < self.expanded_size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "sparse image has a hole in its chunk table",
            ));
        }
        Ok(written)
    }
}

impl<R: Read + Seek> Seek for SparseImage<R> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let next = match from {
            SeekFrom::Start(n) => i128::from(n),
            SeekFrom::Current(n) => i128::from(self.position) + i128::from(n),
            SeekFrom::End(n) => i128::from(self.expanded_size) + i128::from(n),
        };
        if !(0..=i128::from(self.expanded_size)).contains(&next) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek outside expanded sparse image",
            ));
        }
        self.position = next as u64;
        Ok(self.position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn header(block_size: u32, blocks: u32, chunks: u32, checksum: u32) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&SPARSE_MAGIC.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(SPARSE_HEADER_SIZE as u16).to_le_bytes());
        out.extend_from_slice(&(SPARSE_CHUNK_HEADER_SIZE as u16).to_le_bytes());
        out.extend_from_slice(&block_size.to_le_bytes());
        out.extend_from_slice(&blocks.to_le_bytes());
        out.extend_from_slice(&chunks.to_le_bytes());
        out.extend_from_slice(&checksum.to_le_bytes());
        out
    }

    fn chunk(kind: u16, blocks: u32, data: &[u8], output_block_size: u32) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&blocks.to_le_bytes());
        out.extend_from_slice(&((SPARSE_CHUNK_HEADER_SIZE + data.len()) as u32).to_le_bytes());
        out.extend_from_slice(data);
        let _ = output_block_size;
        out
    }

    #[test]
    fn expands_raw_fill_and_dont_care_chunks_as_a_seekable_stream() {
        let raw = vec![b'R'; 512];
        let fill = [0x11, 0x22, 0x33, 0x44];
        let mut bytes = header(512, 3, 3, 0);
        bytes.extend(chunk(CHUNK_RAW, 1, &raw, 512));
        bytes.extend(chunk(CHUNK_FILL, 1, &fill, 512));
        bytes.extend(chunk(CHUNK_DONT_CARE, 1, &[], 512));
        let mut image =
            SparseImage::parse(Cursor::new(bytes.clone()), 0, bytes.len() as u64).unwrap();
        assert_eq!(image.expanded_size, 1536);
        let mut expanded = Vec::new();
        image.read_to_end(&mut expanded).unwrap();
        assert_eq!(&expanded[..512], raw.as_slice());
        assert!(
            expanded[512..1024]
                .iter()
                .enumerate()
                .all(|(i, &b)| b == fill[i % 4])
        );
        assert!(expanded[1024..].iter().all(|&b| b == 0));
        image.seek(SeekFrom::Start(510)).unwrap();
        let mut middle = [0; 6];
        image.read_exact(&mut middle).unwrap();
        assert_eq!(&middle, &[b'R', b'R', 0x11, 0x22, 0x33, 0x44]);
    }

    #[test]
    fn skips_sparse_chunk_header_extensions_before_fill_payload() {
        let pattern = [0x12, 0x34, 0x56, 0x78];
        let mut bytes = header(512, 1, 1, 0);
        bytes[10..12].copy_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(&CHUNK_FILL.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&20u32.to_le_bytes());
        bytes.extend_from_slice(b"EXT!");
        bytes.extend_from_slice(&pattern);

        let mut image =
            SparseImage::parse(Cursor::new(bytes.clone()), 0, bytes.len() as u64).unwrap();
        let mut expanded = vec![0; 512];
        image.read_exact(&mut expanded).unwrap();
        assert!(
            expanded
                .iter()
                .enumerate()
                .all(|(i, &byte)| byte == pattern[i % 4])
        );
    }

    #[test]
    fn verifies_sparse_chunk_and_image_checksums() {
        let raw = vec![b'1'; 1024];
        let mut crc = Hasher::new();
        crc.update(&raw);
        let expected = crc.finalize();
        let mut bytes = header(512, 2, 2, expected);
        bytes.extend(chunk(CHUNK_RAW, 2, &raw, 512));
        bytes.extend(chunk(CHUNK_CRC32, 0, &expected.to_le_bytes(), 512));
        let image = SparseImage::parse(Cursor::new(bytes.clone()), 0, bytes.len() as u64).unwrap();
        assert_eq!(image.expanded_size, 1024);
        let mut bad = bytes;
        let last = bad.len() - 4;
        bad[last] ^= 1;
        assert!(SparseImage::parse(Cursor::new(bad.clone()), 0, bad.len() as u64).is_err());
    }

    #[test]
    fn rejects_truncated_chunks_and_wrong_output_lengths() {
        let mut truncated = header(512, 1, 1, 0);
        truncated.extend(chunk(CHUNK_RAW, 1, b"123", 512));
        let len = truncated.len() as u64;
        assert!(SparseImage::parse(Cursor::new(truncated), 0, len).is_err());

        let mut wrong = header(512, 2, 1, 0);
        wrong.extend(chunk(CHUNK_RAW, 1, &vec![b'1'; 512], 512));
        let len = wrong.len() as u64;
        assert!(SparseImage::parse(Cursor::new(wrong), 0, len).is_err());
    }

    #[test]
    fn recognizes_real_raw_super_metadata_and_rejects_sparse_non_super_images() {
        let raw = include_bytes!("../../../skyward-lp/tests/ishtar-super-metadata.bin");
        let mut cursor = Cursor::new(raw.as_slice());
        assert!(is_super(&mut cursor, &raw[..4096]).unwrap());

        let sparse = header(512, 16, 1, 0);
        let mut non_super = sparse;
        non_super.extend(chunk(CHUNK_RAW, 16, &vec![0u8; 8192], 512));
        let mut cursor = Cursor::new(non_super.clone());
        assert!(!is_super(&mut cursor, &non_super[..4096]).unwrap());

        let mut empty = Cursor::new(vec![0u8; 8192]);
        assert!(!is_super(&mut empty, &[0; 4096]).unwrap());
    }

    #[test]
    fn raw_super_seek_is_relative_to_its_start_in_a_larger_stream() {
        let raw = include_bytes!("../../../skyward-lp/tests/ishtar-super-metadata.bin");
        let mut source =
            Cursor::new([b"prefix".as_slice(), raw.as_slice(), b"trailer".as_slice()].concat());
        source.seek(SeekFrom::Start(6)).unwrap();
        let mut image = SuperImage::open(source).unwrap();
        let mut magic = [0u8; 4];
        image.read_exact(&mut magic).unwrap();
        assert_eq!(&magic, &raw[..4]);
        image.seek(SeekFrom::Start(0)).unwrap();
        image.read_exact(&mut magic).unwrap();
        assert_eq!(&magic, &raw[..4]);
    }

    #[test]
    fn validates_both_slots_in_a_sparse_full_size_super_image() {
        let raw = include_bytes!("../../../skyward-lp/tests/ishtar-super-metadata.bin");
        let metadata_size = skyward_lp::metadata_prefix_size(raw).unwrap();
        assert_eq!(metadata_size, raw.len());
        let a = skyward_lp::read_bytes(raw, 0).unwrap();
        let partition_size = a.block_devices[0].size;
        assert!(partition_size.is_multiple_of(512));
        let block_size = 512u32;
        let total_blocks = (partition_size / u64::from(block_size)) as u32;
        let raw_blocks = (raw.len() / block_size as usize) as u32;
        assert_eq!(raw_blocks * block_size, raw.len() as u32);
        let mut bytes = header(block_size, total_blocks, 2, 0);
        bytes.extend(chunk(CHUNK_RAW, raw_blocks, raw, block_size));
        bytes.extend(chunk(
            CHUNK_DONT_CARE,
            total_blocks - raw_blocks,
            &[],
            block_size,
        ));

        let mut image = SuperImage::open(Cursor::new(bytes)).unwrap();
        let metadata = image.validate(partition_size).unwrap();
        assert!(metadata[0].partition("system_a").is_some());
        assert!(metadata[1].partition("system_b").is_some());
        assert_eq!(image.expanded_size(), partition_size);
    }
}
