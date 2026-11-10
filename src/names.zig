//! Names for new sessions: two friendly words, like "quiet-river", in place
//! of numbers.

const std = @import("std");
const c = @import("c");

pub const adjectives = [_][]const u8{
    "amber",   "ancient", "autumn",  "bold",    "brave",   "bright",  "brisk",   "calm",
    "clever",  "cosmic",  "cozy",    "crimson", "crisp",   "curious", "daring",  "dawn",
    "eager",   "early",   "easy",    "electric", "emerald", "fancy",  "fearless", "gentle",
    "giddy",   "golden",  "happy",   "hidden",  "humble",  "icy",     "jolly",   "keen",
    "kind",    "lively",  "lucky",   "lunar",   "mellow",  "merry",   "misty",   "noble",
    "nimble",  "patient", "plucky",  "polar",   "positive", "proud",  "quick",   "quiet",
    "rapid",   "royal",   "rustic",  "silent",  "silver",  "snowy",   "solar",   "steady",
    "sunny",   "swift",   "tidy",    "velvet",  "vivid",   "wandering", "wild",  "witty",
};

pub const nouns = [_][]const u8{
    "acorn",   "aurora",  "bay",     "birch",   "breeze",  "brook",   "canyon",  "cedar",
    "cliff",   "cloud",   "comet",   "coral",   "cove",    "creek",   "delta",   "dune",
    "ember",   "fern",    "field",   "fjord",   "forest",  "frost",   "garden",  "glacier",
    "grove",   "harbor",  "hill",    "island",  "lagoon",  "lake",    "leaf",    "maple",
    "meadow",  "mesa",    "moon",    "moss",    "orchard", "peak",    "pebble",  "pine",
    "planet",  "pond",    "prairie", "rain",    "reef",    "ridge",   "river",   "sky",
    "snow",    "spark",   "spring",  "star",    "stone",   "storm",   "summit",  "sun",
    "thunder", "tide",    "trail",   "valley",  "wave",    "willow",  "wind",    "water",
};

/// A random "adjective-noun" name, written into `buf` (32 bytes are plenty).
pub fn random(buf: []u8) []const u8 {
    const a = adjectives[c.arc4random_uniform(adjectives.len)];
    const n = nouns[c.arc4random_uniform(nouns.len)];
    return std.fmt.bufPrint(buf, "{s}-{s}", .{ a, n }) catch unreachable;
}

test "names are two lowercase words" {
    var buf: [48]u8 = undefined;
    for (0..200) |_| {
        const name = random(&buf);
        const dash = std.mem.indexOfScalar(u8, name, '-') orelse return error.NoDash;
        try std.testing.expect(dash > 0 and dash < name.len - 1);
        for (name) |ch| try std.testing.expect(ch == '-' or std.ascii.isLower(ch));
    }
}

test "the lists have no repeats" {
    inline for (.{ adjectives, nouns }) |list| {
        for (list, 0..) |a, i| for (list[i + 1 ..]) |b| try std.testing.expect(!std.mem.eql(u8, a, b));
    }
}
