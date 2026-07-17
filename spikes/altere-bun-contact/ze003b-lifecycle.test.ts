import { expect, test } from "bun:test";
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { once } from "node:events";
import { mkdtemp, mkdir, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { createInterface, type Interface } from "node:readline";

type RotationResponse = {
	id: number;
	result: {
		opened: string;
	};
};

class RotationService {
	readonly #child: ChildProcessWithoutNullStreams;
	readonly #lines: AsyncIterator<string>;
	readonly #readline: Interface;
	#nextId = 1;

	constructor(collectionRoot: string, checkpointPath: string) {
		this.#child = spawn(
			process.execPath,
			[path.join(import.meta.dir, "ze003b-service.ts"), collectionRoot, checkpointPath],
			{
				env: { ...process.env, ZE003_IR_TS: process.env.ZE003_IR_TS },
				stdio: ["pipe", "pipe", "pipe"],
			},
		);
		this.#readline = createInterface({ input: this.#child.stdout });
		this.#lines = this.#readline[Symbol.asyncIterator]();
	}

	async next(): Promise<RotationResponse> {
		const id = this.#nextId++;
		this.#child.stdin.write(`${JSON.stringify({ id, method: "rotation.next" })}\n`);
		const line = await this.#lines.next();
		if (line.done) {
			const stderr = await new Response(this.#child.stderr).text();
			throw new Error(stderr.trim() || "Rotation service exited without a response");
		}
		return JSON.parse(line.value) as RotationResponse;
	}

	async kill(): Promise<void> {
		this.#child.kill("SIGKILL");
		await once(this.#child, "exit");
		this.#readline.close();
	}
}

async function createCollection(): Promise<{ root: string; checkpointPath: string }> {
	const parent = await mkdtemp(path.join(tmpdir(), "altere-ze003b-"));
	const root = path.join(parent, "collection");
	const stateRoot = path.join(root, ".altere");
	await mkdir(stateRoot, { recursive: true });
	await writeFile(
		path.join(root, "one.md"),
		"---\npriority: 10\nreads: 0\n---\n\nOne\n",
	);
	await writeFile(
		path.join(root, "two.md"),
		"---\npriority: 20\nreads: 0\n---\n\nTwo\n",
	);
	return { root, checkpointPath: path.join(stateRoot, "rotation.json") };
}

test("rotation resumes from a collection-owned checkpoint after Bun is killed", async () => {
	const { root, checkpointPath } = await createCollection();
	const firstRuntime = new RotationService(root, checkpointPath);

	const first = await firstRuntime.next();
	expect(path.basename(first.result.opened)).toBe("one.md");
	await firstRuntime.kill();

	const checkpointAfterKill = JSON.parse(await readFile(checkpointPath, "utf8")) as {
		currentPath: string;
	};
	expect(path.basename(checkpointAfterKill.currentPath)).toBe("one.md");

	const secondRuntime = new RotationService(root, checkpointPath);
	const second = await secondRuntime.next();
	const third = await secondRuntime.next();
	await secondRuntime.kill();

	expect(path.basename(second.result.opened)).toBe("two.md");
	expect(path.basename(third.result.opened)).toBe("one.md");
	expect(await readFile(path.join(root, "one.md"), "utf8")).toContain("reads: 1");
	expect(await readFile(path.join(root, "two.md"), "utf8")).toContain("reads: 1");
});
