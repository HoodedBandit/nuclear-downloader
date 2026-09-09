using System;
using System.Drawing;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;

namespace NuclearDownloader.VisualContract
{
    public sealed class PngComparison
    {
        public int Width { get; set; }
        public int Height { get; set; }
        public bool Equal { get; set; }
        public long DifferentPixels { get; set; }
        public int FirstDifferenceX { get; set; } = -1;
        public int FirstDifferenceY { get; set; } = -1;
    }

    public static class RendererBaseline
    {
        public static PngComparison CompareDecodedPixels(string expectedPath, string actualPath)
        {
            using (var expectedSource = new Bitmap(expectedPath))
            using (var actualSource = new Bitmap(actualPath))
            {
                var result = new PngComparison {
                    Width = expectedSource.Width,
                    Height = expectedSource.Height,
                    Equal = expectedSource.Width == actualSource.Width && expectedSource.Height == actualSource.Height
                };
                if (!result.Equal) return result;

                using (var expected = ToArgb(expectedSource))
                using (var actual = ToArgb(actualSource))
                {
                    var rectangle = new Rectangle(0, 0, expected.Width, expected.Height);
                    var expectedData = expected.LockBits(rectangle, ImageLockMode.ReadOnly, PixelFormat.Format32bppArgb);
                    var actualData = actual.LockBits(rectangle, ImageLockMode.ReadOnly, PixelFormat.Format32bppArgb);
                    try
                    {
                        var rowLength = checked(expected.Width * 4);
                        var expectedRow = new byte[rowLength];
                        var actualRow = new byte[rowLength];
                        for (var y = 0; y < expected.Height; y++)
                        {
                            Marshal.Copy(IntPtr.Add(expectedData.Scan0, y * expectedData.Stride), expectedRow, 0, rowLength);
                            Marshal.Copy(IntPtr.Add(actualData.Scan0, y * actualData.Stride), actualRow, 0, rowLength);
                            for (var x = 0; x < expected.Width; x++)
                            {
                                var offset = x * 4;
                                if (expectedRow[offset] == actualRow[offset] &&
                                    expectedRow[offset + 1] == actualRow[offset + 1] &&
                                    expectedRow[offset + 2] == actualRow[offset + 2] &&
                                    expectedRow[offset + 3] == actualRow[offset + 3]) continue;
                                result.DifferentPixels++;
                                if (result.FirstDifferenceX < 0) {
                                    result.FirstDifferenceX = x;
                                    result.FirstDifferenceY = y;
                                }
                            }
                        }
                        result.Equal = result.DifferentPixels == 0;
                        return result;
                    }
                    finally
                    {
                        expected.UnlockBits(expectedData);
                        actual.UnlockBits(actualData);
                    }
                }
            }
        }

        public static int[] GetDecodedDimensions(string path)
        {
            using (var bitmap = new Bitmap(path)) return new[] { bitmap.Width, bitmap.Height };
        }

        private static Bitmap ToArgb(Bitmap source)
        {
            var result = new Bitmap(source.Width, source.Height, PixelFormat.Format32bppArgb);
            using (var graphics = Graphics.FromImage(result)) {
                graphics.CompositingMode = System.Drawing.Drawing2D.CompositingMode.SourceCopy;
                graphics.DrawImageUnscaled(source, 0, 0);
            }
            return result;
        }
    }
}
