# Research: how Bazel 9.2.0 carries symbolic links in action inputs and outputs (source read)

## Metadata

**Date:** 2026-11-25
**Lane:** disputed-fact check, source read (/hex-discuss, .agents/discussions/bazel-rbe-rules-ocx.md)
**Basis:** READ, not run. Bazel source at tag 9.2.0 (commit 8220c61), compared with tag 9.3.3 (commit b592da4, fetched shallow, so no commit history between the two was available). The GitHub issue tracker was searched read-only.
**Expires:** 2027-05-25
**Caveat:** findings only, no recommendation. Nothing here was executed.

All paths below are relative to `src/main/java/com/google/devtools/build/lib/` and refer to tag 9.2.0 unless a section says otherwise. Statements marked INFERRED are my reading of how pieces fit together, not a line I can point at.

## 1. Tree artifact metadata after a local action

The metadata of a tree artifact is built by `skyframe/ActionOutputMetadataStore.java`, method `constructTreeArtifactValueFromFilesystem` (lines 244-317). It walks the directory with `TreeArtifactValue.visitTree` (`skyframe/TreeArtifactValue.java:611`) and stores one entry per regular file in the tree value. Directories are skipped, with the comment "The final TreeArtifactValue does not contain child directories" (`ActionOutputMetadataStore.java:273`).

Symbolic links are never visible to the visitor. The visitor interface states that links resolving to an existing file or directory are "followed and reported as the regular files or directories they point to", and that "the visitor is never called with a Dirent.Type.SYMLINK type" (`TreeArtifactValue.java:492-496`). The implementation is in `TreeArtifactValue.Visitor.visit` (lines 540-585): for a link it sets `traversedSymlink = true` (554), calls `statIfFound(Symlinks.FOLLOW)`, and maps the result to FILE or DIRECTORY. The cases are:

- Link to a file: the entry is reported as FILE under the link's own path. The file metadata is then built for the link path. In `ActionOutputMetadataStore.fileArtifactValueFromArtifact` a link is resolved with `resolveSymbolicLinks()` (line 515), the value is made from the real file's stat (line 527-529), and the digest is computed on the real path (lines 440-448). The result is a normal file value, plus a `resolvedPath` hint pointing at the real file (lines 537-541). The link itself is not recorded.
- Link to a directory: reported as DIRECTORY, then `readdir(Symlinks.NOFOLLOW)` is called on the link path (line 580), which the file system follows. The directory's contents appear as ordinary files under the link's apparent path. A linked directory therefore turns into duplicated child entries.
- Dangling link: throws `IOException("child ... is a dangling symbolic link")` (lines 556-561), so the action fails.
- Absolute link, or a link leaving the tree: handled exactly like a relative link that resolves. The only difference is that output permissions are not set on entries reached through a link (`ActionOutputMetadataStore.java:268-271`). The doc comment on `visitTree` says a symlink that "traverses outside of the tree artifact" is invalid (`TreeArtifactValue.java:601-603`), but I found no code in the visitor or in `ActionOutputMetadataStore` that enforces it at 9.2.0 (grep for "outside" finds only that comment and the permissions comment). INFERRED: not enforced.

So there is no representation of an internal link in the tree value. The value holds a sorted map from child path to file metadata (`TreeArtifactValue.java` `childData`). `--allow_unresolved_symlinks` (default true, `analysis/config/CoreOptions.java:878`) does not change this: its help text says "Unresolved symlinks inside tree artifacts are not currently supported" (line 892).

## 2. Remote execution input root (Merkle tree)

The input root is built by `remote/merkletree/MerkleTreeComputer.java`, entry point `buildForSpawn` (line 237) and `doBuildForSpawn` (line 279). The per-node decision is the `switch (input)` in `buildWithPrecomputedSubTrees` (lines 611-705).

(a) A child of a tree artifact that is a link on disk: a file node. The tree artifact becomes a sub-tree built by `computeForTreeArtifactIfAbsent` (line 865) from the tree value's children. Each child is a `TreeFileArtifact`, so it falls to `case Artifact fileOrSourceDirectory` (line 633) and `addFile(...)` with the digest from the metadata. Because section 1 shows the metadata holds the resolved file, the link is already gone before this point. There is no symlink node for tree children.

(b) A source file in an external repository that is a relative link on disk: a file node. The source artifact value comes from `FileArtifactValue.createForSourceArtifact(artifact, fileValue, ...)` (`actions/FileArtifactValue.java:301-315`), which uses `fileValue.isFile()`, `getSize()` and `getDigest()` of the Skyframe `FileValue`. The file value is the resolved file. The Merkle code then takes `case Artifact fileOrSourceDirectory` (line 633) and, because the type is not DIRECTORY, adds a file node (lines 663-672). Not read in detail: how `FileValue` resolves a link into an external repo (it lives in Skyframe's file function, outside the files I read).

(c) A source directory given as an input: a directory node whose contents are exploded at Merkle time. `maybeCacheSubtree` (line 780-791) calls `explodeDirectory` (line 1069-1096), which does `readdir(Symlinks.FOLLOW)` (line 1086). Files, and links to files, become `ChildActionInput` entries; links to directories are recursed into; anything else throws "The file type ... is not supported" (line 1093-1095). The children are then added through the generic loop, where a bare input falls to `default` (lines 697-705) and is digested by `digestUtil.compute(artifactPathResolver.toPath(input))`, which follows links. Result: only file and directory nodes, no symlink nodes. A dangling link throws.

(d) An unresolved symlink artifact from `ctx.actions.declare_symlink`: a symlink node. `case Artifact.SpecialArtifact symlink when symlink.isSymlink()` (line 619-632) reads `metadata.getUnresolvedSymlinkTarget()` and calls `addSymlinksBuilder()`. The metadata is created by `FileArtifactValue.createForUnresolvedSymlink` (`ActionOutputMetadataStore.java:486-491`), a readlink, with no resolution.

(e) The same artifact inside a tool's runfiles tree: also a symlink node. A runfiles tree is built as a sub-tree by `computeForRunfilesTreeIfAbsent` (line 810-845) from `getRunfilesTree().getMapping()`, and the sub-tree is built by the same `build(...)` and the same switch, so line 619 applies to each mapped entry. INFERRED only in that I did not follow how the mapping is populated for a symlink artifact; the switch itself is read.

Net effect on the observed behaviour: nothing in the 9.2.0 input root keeps a link that lives inside a tree artifact, but nothing is lost either, because the links are not links by the time they reach the input root.

## 3. Upload of outputs to a remote cache

A locally executed spawn's outputs are described by `remote/UploadManifest.java`. A tree artifact is a real directory, so `addFiles` calls `addDirectory` (line 556-566 in 9.2.0; method `addFiles` at about line 240), which runs a `DirectoryBuilder` (line 390 onward). Its `visit` (line 470-517) handles each link as follows:

- Dangling or looping link: uploaded as a symlink node (`visitAsSymlink`, line 498).
- Relative link whose target exists: also uploaded as a symlink node (condition `statFollow == null || !target.isAbsolute()`, line 493). This is the case for the node and python links, including targets with `../`. The class comment confirms: "non-dangling relative symlinks ... are uploaded as such" (lines 228-230).
- Absolute link whose target exists: uploaded as the file or directory it points to (lines 500-509).
- Absolute dangling link: uploaded as a symlink only if the server allows it, checked by `checkAbsoluteSymlinkAllowed` (line 570-577) against `cacheCapabilities.getSymlinkAbsolutePathStrategy()` (line 128-130).

Flags: I found no Bazel flag that gates any of this at 9.2.0. `RemoteOptions.java` contains a single symlink option, `remote_download_symlink_template` (line 595, default empty), which concerns downloads. The only gate is the server's `SymlinkAbsolutePathStrategy` capability, and only for absolute dangling links. The rule is the same in 9.3.3: the upload manifest diff between 9.2.0 and 9.3.3 only adds a `force` parameter and digest overloads.

## 4. Download without the bytes (default `--remote_download_outputs=toplevel`)

The handler is `RemoteExecutionService.downloadOutputs` (`remote/RemoteExecutionService.java:1206`). The cached action result is parsed into file entries and symlink entries by `parseDirectory` (lines 1060-1090); symlink nodes of a tree become `SymlinkMetadata` and are not file entries.

For each file inside an output directory, `shouldDownload(result, path, treeRootExecPath)` is asked (line 1319). With mode `toplevel`, `RemoteOutputChecker.shouldDownloadOutput` returns true only for paths in `pathsToDownload` or matching `--remote_download_regex` (`remote/RemoteOutputChecker.java:328-337`); an intermediate tree is neither. So the file is not downloaded and instead `remoteActionFileSystem.injectRemoteFile(...)` records metadata only (lines 1329-1334). In 9.2.0 the file system is `RemoteActionFileSystem`, which keeps these in an in-memory tree (`remote/RemoteActionFileSystem.java:83-100`).

Symlink nodes are always materialised on disk, regardless of the download mode: after the downloads, `createSymlinks(symlinks)` (line 1399; body at 926-949) creates every link of every directory with `createSymbolicLink(target)`, creating parent directories first (line 927-931). So after a cache hit with `toplevel`: the links exist on disk (dangling, because their targets were not downloaded), and the regular files exist only as remote metadata. INFERRED: the tree value that Skyframe then computes lists the link paths as files, because `RemoteActionFileSystem.statInternal` resolves the on-disk link to the remote metadata of the target (`RemoteActionFileSystem.java:660-703`) and the visitor in section 1 reports a resolving link as a FILE. That makes each link path a tree child with the target's digest.

With `--remote_download_outputs=all`, `shouldDownload` returns true for every file (`RemoteOutputChecker.java:333`), the files are written first, and the links are created afterwards (comment at line 1396-1398), which is why all links come back.

## 5. Later materialisation by the input prefetcher

The spawn runner calls `prefetchInputs` (`exec/AbstractSpawnStrategy.java:273`, called from `sandbox/AbstractSandboxSpawnRunner.java:102-103`). It passes `getInputMapping(...).values()`, a `TreeMap<PathFragment, ActionInput>` (`exec/SpawnInputExpander.java:214`) whose iteration order is plain string order of the full path (`PathFragment.compareTo`, `vfs/PathFragment.java:514-516`). Tree artifacts are expanded into one `TreeFileArtifact` per child (`SpawnInputExpander.java` `addInputs`, `TreeFileArtifact` case). The fetcher is `remote/RemoteActionInputFetcher` over `remote/AbstractActionInputPrefetcher.java`.

For each input, in that sorted order, `prefetchFile` (line 410 onward) calls `downloadFileNoCheckRx` (line 600). The steps that matter:

1. For a tree child, only the tree root is resolved with `maybeResolveSymlink(treeRoot)` (line 617). The child path itself is not resolved. For a non-tree input the path is resolved (line 620), which means a symlink at a non-tree path would be written through to its target. The child of a tree is the case here, so the child path stays the link path.
2. `shouldDownloadFile(finalPath, metadata)` (lines 254-280; called at line 673) runs `path.statIfFound()` (line 256), which follows links (`vfs/Path.java:365-367`, `followSymlinks=true`). If the stat is null (dangling link or missing path), it downloads. If the size differs, it downloads. Otherwise it compares digests and skips the download if they match.
3. A download writes to a temp file and `finalizeDownload` calls `FileSystemUtils.moveFile(tmpPath, finalPath)` (line 728). `moveFile` is a rename that "does not follow symbolic links" and may overwrite the destination (`vfs/FileSystemUtils.java:436-460`); a rename over a link replaces the link itself with the regular file. (That rename replaces the link, not its target, is standard POSIX behaviour; INFERRED that the native file system here behaves so.)

This explains the observed pattern. A child whose on-disk link is still dangling when it is checked (the target has not been written yet) fails the stat, is downloaded, and the link is replaced by a regular file with the target's content. A child whose link already resolves to an earlier-written file with the same digest and size passes `shouldDownloadFile` and is skipped, so the link survives. Because inputs are visited in plain string order of the full path, "the target sorts before the link name" is the survival condition:

- `bin/npm -> ../lib/node_modules/npm/bin/npm-cli.js`: `bin/...` sorts before `lib/...`, so the target is not yet present, and the link is replaced. All 3 node links have this shape. Matches 0 of 3 kept.
- Links in `bin/` and `lib/` with `../` targets: same reason, replaced.
- Links in `share/terminfo` pointing at a file in the same directory with a name that sorts earlier: the target was visited first, so it exists, and the link is kept. Matches 291 of 1048.

Two things I could not establish from the code. First, the survival condition also needs the earlier target's download to have finished before the link's `shouldDownloadFile` runs; downloads are asynchronous (the check is inside `Completable.defer`, lines 671-678), so I cannot say from the code alone whether this is deterministic or a timing race that merely wins for small files from a disk cache. Second, the exact tree children list in step 0 (INFERRED in section 4). The pattern of 291 survivors that all sort target-first is consistent with the code but is not proof of determinism.

The link plant path (`plantSymlink`, lines 780-790, `linkPath.delete()` at 785) is used only for artifacts with a `resolvedPath` hint or for external-repo chains (`getSymlinks`, lines 480-527), not for internal links of a tree.

## 6. Local sandbox staging

Both the processwrapper sandbox and the non-hermetic linux sandbox use `SymlinkedSandboxedSpawn` (`sandbox/ProcessWrapperSandboxedSpawnRunner.java:95`; `sandbox/LinuxSandboxedSpawnRunner.java:368`). `AbstractContainerizingSandboxedSpawn.createInputs` (lines 150-178) calls `copyFile(fileDest, key)`, which for the symlinked variant is `target.createSymbolicLink(source)` (`sandbox/SymlinkedSandboxedSpawn.java:129`). `SandboxHelpers.processInputFiles` (lines 665-695) fills the maps.

- (a) Children of a tree artifact: one symlink per child file, pointing at the exec-root path of that child (`inputFiles.put(pathFragment, execRoot.getRelative(execPath))`, line 691). The sandbox symlink points at the exec-root path, which can itself be a link, so access goes through two links. A tree child that is a link on disk is therefore staged as a link to a link, and a link to a directory inside the tree is not staged as one directory link: its expanded files are staged individually.
- (b) A source directory: one entry, so one symlink to the directory in the exec root (the input mapping holds the directory artifact as a single value; `SpawnInputExpander.addInputs`, default case). INFERRED from the single map entry; I did not find code in the sandbox that expands it.
- (c) A runfiles tree: expanded in the input mapping (`SpawnInputExpander.addSingleRunfilesTreeToInputs`, lines 76-135) into one entry per runfile, with tree artifacts expanded per child; each becomes one symlink per file.
- (d) An unresolved symlink artifact: copied as a link. `processInputFiles` checks `actionInput.isSymlink()` (line 683) and stores `inputPath.readSymbolicLink()` (line 685); `createInputs` then does `key.createSymbolicLink(symlinkDest)` (lines 171-175). This also applies to one reached through a runfiles tree, where the mapping carries the artifact.

The hermetic linux sandbox (`--experimental_use_hermetic_linux_sandbox`, default false) uses `HardlinkedSandboxedSpawn`, which hard-links files recursively and "Symlinks are resolved" (`sandbox/HardlinkedSandboxedSpawn.java:75-85`, 103-113): a different answer for all of (a) to (c). It is not what the observed runs used.

## 7. Flags that change any answer above (9.2.0)

- `--remote_download_outputs` (`RemoteOptions.java:492`), default `toplevel`. `minimal` downloads even less, `all` downloads every file at action completion, which is why all links return (section 4). Convenience forms `--remote_download_minimal`, `--remote_download_toplevel`, `--remote_download_all` (lines 515-541) set it.
- `--remote_download_regex` (`remote/options/CommonRemoteOptions.java:32`), default null, repeatable. Files whose path, or whose tree root's path, matches are downloaded in all modes (`RemoteOutputChecker.java:335-336`).
- `--remote_download_symlink_template` (`RemoteOptions.java:595`), default empty. Replaces downloads with symlinks to a templated target. Not in play here.
- `--allow_unresolved_symlinks` (`CoreOptions.java:878`), default true. Enables `declare_symlink` and `ctx.actions.symlink` without a target. Does not allow unresolved symlinks inside tree artifacts.
- `--reuse_sandbox_directories` (`sandbox/SandboxOptions.java:310`), default true, and `--experimental_inmemory_sandbox_stashes` (line 322), default false. Reuse the sandbox directory between actions; the reuse logic (`SandboxHelpers.cleanExisting`, used at `SymlinkedSandboxedSpawn.java:90-115`) leaves already-correct symlinks in place. I did not read `cleanExisting` in detail.
- `--experimental_use_hermetic_linux_sandbox` (`SandboxOptions.java:334`), default false. Switches the linux sandbox to hard links with links resolved (section 6).
- `--sandbox_base` (line 100), default empty, and `--experimental_sandboxfs_map_symlink_targets` (line 183), default false. Sandbox location and a sandboxfs-only link mapping; no effect on the answers I traced.
- `--experimental_remote_discard_merkle_trees` (`RemoteOptions.java:641`), default true. Whether Merkle trees are kept (blob policy); it does not change node types.

No flag changes which node type an internal link in a tree gets on upload or in the input root.

## 8. Later versions, and the issue tracker

I compared 9.2.0 with 9.3.3 (the newest 9.x release tag at the time; 9.3.1, 9.3.2 and release candidates also exist). Only the diff was available, not the history, so I cannot name commits.

- Unchanged in a way that matters: `skyframe/TreeArtifactValue.java`, `UploadManifest.java` link rules, `RemoteExecutionService.createSymlinks`, `downloadOutputs`, `RemoteActionFileSystem` symlink logic, `SymlinkedSandboxedSpawn`, and the options in section 7. The `shouldDownloadFile` logic of the prefetcher is unchanged except that, when it skips a download, it now records the contents proxy (`AbstractActionInputPrefetcher.java`, 9.3.3 diff hunk at line 277). The skip-on-digest-match path that explains the surviving links is still there.
- `exec/SpawnInputExpander.java`: the input mapping changes from `new TreeMap<>()` to `new TreeMap<>(HIERARCHICAL_COMPARATOR)`. This changes the visiting order of paths so that children of a directory sort directly after it (for example `foo/bar/quz` before `foo/bar-baz/quz`). It can change which links see their target first, but only for paths that differ in a character that sorts between `/` and other characters; I did not work out the effect on the node or python layouts.
- `AbstractActionInputPrefetcher.prefetchFile` adds an early return for directory-typed metadata (source directories in external repos) and a `LostInputsExecException` path in `finalizeAction`. Neither touches links in tree children.
- `MerkleTreeComputer`: the persistent sub-tree cache key for tree artifacts changes from `treeArtifactValue.getMetadata()` to the `treeArtifactValue` itself, with a comment that the old call "returns a new instance on every call and is thus unusable as an identity-compared weak key". See section 9.
- `SandboxHelpers.java`, `ActionOutputMetadataStore.java` and `RemoteOptions.java` also differ; I looked at the option additions (gRPC service config, download idle timeout, chunking) and the Merkle and prefetch hunks, and saw nothing about links. I did not read the `SandboxHelpers` or `ActionOutputMetadataStore` hunks line by line.

Issue tracker (read-only search of `bazelbuild/bazel`, my own search terms; the search tool matches narrowly, so absence is weak evidence):

- No issue found that describes the exact pattern in sections 4 and 5 (links in a tree artifact becoming regular files after a cache hit with `toplevel`).
- [#21171](https://github.com/bazelbuild/bazel/issues/21171), closed: an `IOException` on a tree artifact that contains a symlink to a directory with `--remote_download_outputs=toplevel` when the producing action is a cache hit; the workaround listed is `--remote_download_outputs=all`. Related mechanism, different symptom. Closed after an attempted fix in 7.1.0; later comments say it persisted for some users.
- [#20891](https://github.com/bazelbuild/bazel/issues/20891), closed: requests links with `..` inside tree artifacts, citing the check in `TreeArtifactValue.java`.
- [#30352](https://github.com/bazelbuild/bazel/issues/30352), open: relative symlink targets from a remote cache can escape the exec root on download; concerns `createSymlinks`.
- [#32416](https://github.com/bazelbuild/bazel/issues/32416), open: repeated symlink materialisation races with concurrent local actions (remote repo contents cache).

## 9. Merkle tree cost

Yes, partly. `MerkleTreeComputer` keeps two static, cross-action, cross-build caches, `persistentToolSubTreeCache` and `persistentNonToolSubTreeCache` (lines 165-168), built with `Caffeine.newBuilder().weakKeys()`; weak keys compare by identity. They hold the root digest of an aggregate input and are used for tree artifacts, runfiles trees and source directories (`maybeCacheSubtree`, lines 750-800; `computeIfAbsent`, line 895 onward). The keys are:

- Runfiles: `runfilesArtifactValue.getMetadata()` (line 831), a field of a retained value, so the cache can hit.
- Source directory: the metadata from `getInputMetadata(artifact)` (line 779-787), retained by Skyframe, so it can hit.
- Tree artifact: `treeArtifactValue.getMetadata()` (line 866), which `TreeArtifactValue.getMetadata()` implements as `new TreeArtifactCompositeFileArtifactValue(...)` on every call (`skyframe/TreeArtifactValue.java:324-326`). With identity-compared weak keys, a fresh object can never match a stored entry. The 9.3.3 source says exactly this in its comment. INFERRED from Caffeine's weak-key semantics (not a line in Bazel): in 9.2.0 the persistent cache does not hit for tree artifacts, so each action that takes a tree as input rebuilds that tree's sub-Merkle-tree (a sort and a directory proto per directory), although concurrent builds of the same tree are deduplicated through the in-flight table, whose key is a record compared by value (`InFlightCacheKey`, line 195-200).

Individual source files are never cached: they go through the main loop (lines 527-705), one iteration each. For an action with several thousand individual files the work is linear in the number of files, plus a sort: `ImmutableList.sortedCopyOf` over all inputs (line 311, O(n log n)), `precomputeSubTrees` visiting every entry (line 722-748), one pass of the loop that computes each file's node, and one directory proto and digest per directory pop (lines 540-585). Per-file metadata comes from retained Skyframe values (no hashing of content), so the per-file cost is small, but it is not constant.

## Not determined

- Whether the survival of links with an earlier-sorting target is deterministic or depends on download timing (section 5).
- The exact tree-children list produced after a cache hit under `RemoteActionFileSystem` (section 4, INFERRED).
- How `FileValue` resolves a link for a source file in an external repository (section 2b).
- Commits between 9.2.0 and 9.3.3 (shallow clone; no history).
- Whether `SandboxHelpers.cleanExisting` changes link handling when sandbox directories are reused (section 7).
