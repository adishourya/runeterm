const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});

    // spooky links a prebuilt libghostty-vt rather than depending on the
    // ghostty package so that iterating on spooky never rebuilds ghostty.
    // By default it's the one runeterm builds into ../vendor/ghostty-vt
    // (see the README); like runeterm, GHOSTTY_VT_PREFIX overrides it.
    const vt_prefix = b.option(
        []const u8,
        "vt-prefix",
        "Where libghostty-vt is installed (default: $GHOSTTY_VT_PREFIX or ../vendor/ghostty-vt)",
    ) orelse b.graph.environ_map.get("GHOSTTY_VT_PREFIX") orelse "../vendor/ghostty-vt";
    const ghostty_include = b.pathJoin(&.{ vt_prefix, "include" });
    const ghostty_lib = b.pathJoin(&.{ vt_prefix, "lib" });

    const c = b.addTranslateC(.{
        .root_source_file = b.path("src/c.h"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    c.addIncludePath(.{ .cwd_relative = ghostty_include });

    const exe_mod = b.createModule(.{
        .root_source_file = b.path("src/main.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    exe_mod.addImport("c", c.createModule());
    exe_mod.addObjectFile(.{ .cwd_relative = b.pathJoin(&.{ ghostty_lib, "libghostty-vt.a" }) });
    exe_mod.linkSystemLibrary("util", .{});

    const exe = b.addExecutable(.{
        .name = "spooky",
        .root_module = exe_mod,
    });
    b.installArtifact(exe);

    const run_cmd = b.addRunArtifact(exe);
    run_cmd.step.dependOn(b.getInstallStep());
    run_cmd.addPassthruArgs();
    b.step("run", "Run spooky").dependOn(&run_cmd.step);

    const tests = b.addTest(.{ .root_module = exe_mod });
    b.step("test", "Run unit tests").dependOn(&b.addRunArtifact(tests).step);
}
