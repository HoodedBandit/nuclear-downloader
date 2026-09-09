const PNG_SIGNATURE = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);

export function pngDimensions(bytes) {
  if (!Buffer.isBuffer(bytes) || bytes.length < 24) {
    throw new Error('Screenshot is too short to contain a PNG IHDR chunk.');
  }
  if (!bytes.subarray(0, PNG_SIGNATURE.length).equals(PNG_SIGNATURE)) {
    throw new Error('Screenshot does not have the PNG signature.');
  }
  if (bytes.readUInt32BE(8) !== 13 || bytes.subarray(12, 16).toString('ascii') !== 'IHDR') {
    throw new Error('Screenshot does not start with a valid PNG IHDR chunk.');
  }
  const width = bytes.readUInt32BE(16);
  const height = bytes.readUInt32BE(20);
  if (width === 0 || height === 0) throw new Error('Screenshot PNG dimensions must be positive.');
  return { width, height };
}
