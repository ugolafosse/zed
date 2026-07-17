import { afterEach, expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { nextPath } from "./next";

let scratch: string | undefined;

afterEach(async () => {
	if (scratch) await rm(scratch, { force: true, recursive: true });
	scratch = undefined;
});

test("alternates between two Markdown buffers", async () => {
	scratch = await mkdtemp(path.join(tmpdir(), "altere-zed-contact-"));
	const statePath = path.join(scratch, "index");
	const buffers = [path.join(scratch, "one.md"), path.join(scratch, "two.md")];

	expect(await nextPath(buffers, statePath)).toBe(buffers[0]);
	expect(await nextPath(buffers, statePath)).toBe(buffers[1]);
});
