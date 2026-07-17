import { tmpdir } from "node:os";
import path from "node:path";

export async function nextPath(
	buffers: readonly string[],
	statePath: string,
): Promise<string> {
	if (buffers.length === 0) throw new Error("At least one buffer is required");

	let previous = -1;
	try {
		previous = Number.parseInt(await Bun.file(statePath).text(), 10);
	} catch {
		previous = -1;
	}

	const next = Number.isFinite(previous) ? (previous + 1) % buffers.length : 0;
	await Bun.write(statePath, String(next));
	return buffers[next];
}

if (import.meta.main) {
	const fixtureRoot = path.join(import.meta.dir, "fixtures");
	const selected = await nextPath(
		[path.join(fixtureRoot, "one.md"), path.join(fixtureRoot, "two.md")],
		path.join(tmpdir(), "altere-zed-contact-index"),
	);
	console.log(selected);
}
