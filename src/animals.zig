//! The animal each session shows beside its name in ghostty's tab bar.
//!
//! A session gets a random one when it's created and keeps it, so it
//! survives restarts and is the same in every window. `spooky animal`
//! changes it. The glyphs are colour emoji: animals, then sports. The first
//! sixteen keep the order the old Nerd Font icons had, so saved sessions keep
//! their animal.

const std = @import("std");
const c = @import("c");

pub const Animal = struct {
    name: []const u8,
    glyph: []const u8,
};

pub const all = [_]Animal{
    .{ .name = "paw", .glyph = "🐾" },
    .{ .name = "dog", .glyph = "🐶" },
    .{ .name = "cat", .glyph = "🐱" },
    .{ .name = "rabbit", .glyph = "🐰" },
    .{ .name = "owl", .glyph = "🦉" },
    .{ .name = "panda", .glyph = "🐼" },
    .{ .name = "penguin", .glyph = "🐧" },
    .{ .name = "turtle", .glyph = "🐢" },
    .{ .name = "duck", .glyph = "🦆" },
    .{ .name = "elephant", .glyph = "🐘" },
    .{ .name = "koala", .glyph = "🐨" },
    .{ .name = "bird", .glyph = "🐦" },
    .{ .name = "jellyfish", .glyph = "🪼" },
    .{ .name = "bee", .glyph = "🐝" },
    .{ .name = "dolphin", .glyph = "🐬" },
    .{ .name = "snail", .glyph = "🐌" },
    .{ .name = "fox", .glyph = "🦊" },
    .{ .name = "tiger", .glyph = "🐯" },
    .{ .name = "lion", .glyph = "🦁" },
    .{ .name = "frog", .glyph = "🐸" },
    .{ .name = "octopus", .glyph = "🐙" },
    .{ .name = "unicorn", .glyph = "🦄" },
    .{ .name = "whale", .glyph = "🐳" },
    .{ .name = "hedgehog", .glyph = "🦔" },
    .{ .name = "sloth", .glyph = "🦥" },
    .{ .name = "otter", .glyph = "🦦" },
    .{ .name = "parrot", .glyph = "🦜" },
    .{ .name = "crab", .glyph = "🦀" },
    .{ .name = "butterfly", .glyph = "🦋" },
    .{ .name = "dino", .glyph = "🦕" },
    .{ .name = "monkey", .glyph = "🐵" },
    .{ .name = "shark", .glyph = "🦈" },
    .{ .name = "soccer", .glyph = "⚽" },
    .{ .name = "basketball", .glyph = "🏀" },
    .{ .name = "football", .glyph = "🏈" },
    .{ .name = "baseball", .glyph = "⚾" },
    .{ .name = "tennis", .glyph = "🎾" },
    .{ .name = "volleyball", .glyph = "🏐" },
    .{ .name = "rugby", .glyph = "🏉" },
    .{ .name = "pingpong", .glyph = "🏓" },
    .{ .name = "badminton", .glyph = "🏸" },
    .{ .name = "hockey", .glyph = "🏒" },
    .{ .name = "cricket", .glyph = "🏏" },
    .{ .name = "golf", .glyph = "⛳" },
    .{ .name = "boxing", .glyph = "🥊" },
    .{ .name = "bowling", .glyph = "🎳" },
    .{ .name = "pool", .glyph = "🎱" },
    .{ .name = "darts", .glyph = "🎯" },
    .{ .name = "surf", .glyph = "🏄" },
    .{ .name = "bike", .glyph = "🚴" },
    .{ .name = "ski", .glyph = "🎿" },
    .{ .name = "trophy", .glyph = "🏆" },
    .{ .name = "medal", .glyph = "🥇" },
};

/// An index into `all`.
pub const Index = u8;

pub fn find(name: []const u8) ?Index {
    for (all, 0..) |a, i| {
        if (std.mem.eql(u8, a.name, name)) return @intCast(i);
    }
    return null;
}

pub fn random() Index {
    return @intCast(c.arc4random_uniform(all.len));
}

/// Split a session name from the animal that may follow it after a zero
/// byte, as `new` and `open` requests send them: "NAME" or "NAME\x00ANIMAL".
pub fn splitName(payload: []const u8) struct { name: []const u8, animal: []const u8 } {
    const sep = std.mem.indexOfScalar(u8, payload, 0) orelse return .{ .name = payload, .animal = "" };
    return .{ .name = payload[0..sep], .animal = payload[sep + 1 ..] };
}

test "find and splitName" {
    try std.testing.expectEqual(@as(?Index, 0), find("paw"));
    try std.testing.expectEqual(@as(?Index, null), find("dragonfly"));
    try std.testing.expectEqual(@as(?Index, 2), find("cat"));
    const both = splitName("work\x00owl");
    try std.testing.expectEqualStrings("work", both.name);
    try std.testing.expectEqualStrings("owl", both.animal);
    const plain = splitName("work");
    try std.testing.expectEqualStrings("work", plain.name);
    try std.testing.expectEqualStrings("", plain.animal);
}
