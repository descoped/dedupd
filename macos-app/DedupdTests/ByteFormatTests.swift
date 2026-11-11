import Testing

@testable import Dedupd

@Suite("Sizes read as the command line writes them")
struct ByteFormatTests {
    @Test("Whole bytes below a kilobyte", arguments: [(0, "0 B"), (1, "1 B"), (999, "999 B")])
    func bytes(value: UInt64, text: String) {
        #expect(ByteFormat.string(value) == text)
    }

    @Test(
        "Decimal units with two decimals",
        arguments: [
            (1000, "1.00 KB"),
            (1_500_000, "1.50 MB"),
            (105_460_000, "105.46 MB"),
            (290_040_000_000, "290.04 GB"),
            (8_000_000_000_000, "8.00 TB"),
            // Nothing above terabytes: the unit stays and the number grows.
            (8_000_000_000_000_000, "8000.00 TB"),
        ]
    )
    func units(value: UInt64, text: String) {
        #expect(ByteFormat.string(value) == text)
    }

    @Test("One file is singular")
    func files() {
        #expect(ByteFormat.files(1) == "1 file")
        #expect(ByteFormat.files(0) == "0 files")
        #expect(ByteFormat.files(61) == "61 files")
    }
}
