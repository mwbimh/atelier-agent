#!/usr/bin/env node

const assert = require('assert');
const childProcess = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');

const repoRoot = path.resolve(__dirname, '..', '..', '..', '..', '..', '..');

function read(relativePath) {
    return fs.readFileSync(path.join(repoRoot, relativePath), 'utf8');
}

function assertDoesNotMatch(source, pattern, label) {
    assert.ok(!pattern.test(source), `${label} still matches ${pattern}`);
}

const ps1Launcher = read('atelier.ps1');
const cmdLauncher = read('atelier.cmd');

assert.ok(
    ps1Launcher.indexOf('"release"') < ps1Launcher.indexOf('"debug"'),
    'atelier.ps1 must prefer release before debug',
);
assert.match(cmdLauncher, /if not defined PROFILE set "PROFILE=release"/);
assert.match(cmdLauncher, /for %%P in \(release release-dist debug\)/);
for (const [name, source] of [['atelier.ps1', ps1Launcher], ['atelier.cmd', cmdLauncher]]) {
    assertDoesNotMatch(source, /atelier-command-runner/i, name);
    assertDoesNotMatch(source, /atelier-workspace-worker/i, name);
}

const installerPaths = [
    'crates/codegen/atelier-pager/scripts/install.sh',
    'crates/codegen/atelier-pager/scripts/install-enterprise.sh',
    'crates/codegen/atelier-pager/scripts/install-enterprise.ps1',
];
const forbiddenInstallerPatterns = [
    /auth\.json/i,
    /auth\.x\.ai/i,
    /storage\.googleapis/i,
    /ATELIER_DEPLOYMENT_KEY/,
    /deployment\/config/i,
    /managed_config/i,
    /requirements\.toml/i,
];
for (const installerPath of installerPaths) {
    const source = read(installerPath);
    assert.match(source, /ATELIER_RELEASE_BASE_URL/, `${installerPath} must use an explicit release base URL`);
    for (const pattern of forbiddenInstallerPatterns) {
        assertDoesNotMatch(source, pattern, installerPath);
    }
}

const assembler = read('crates/codegen/atelier-pager/npm/atelier/scripts/assemble-platform-packages.js');
assert.match(assembler, /target', 'release', 'atelier'/);
assert.match(assembler, /THIRD_PARTY_NOTICES\.md/);

const postinstall = read('crates/codegen/atelier-pager/npm/atelier/bin/postinstall.js');
assert.match(postinstall, /platform package[\s\S]*process\.exit\(1\)/);
assert.match(postinstall, /if \(!installBinary\([\s\S]*process\.exit\(1\)/);
assert.match(postinstall, /failed to parse existing config/i);
assertDoesNotMatch(postinstall, /catch\s*\{\s*\}\s*\nobj\.cli/, 'postinstall config parsing');

function runPostinstallFixture({ withPlatformPackage = true, withBinary, config }) {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'atelier-postinstall-'));
    const home = path.join(root, 'home');
    const nodeModules = path.join(root, 'node_modules');
    const platformPackage = path.join(
        nodeModules,
        '@atelier',
        `atelier-${process.platform}-${process.arch}`,
    );
    const tomlPackage = path.join(nodeModules, '@iarna', 'toml');
    fs.mkdirSync(tomlPackage, { recursive: true });
    fs.writeFileSync(
        path.join(tomlPackage, 'package.json'),
        JSON.stringify({ name: '@iarna/toml', version: '0.0.0', main: 'index.js' }),
    );
    fs.writeFileSync(
        path.join(tomlPackage, 'index.js'),
        [
            "exports.parse = function parse(source) {",
            "  if (source === '[cli\\ninvalid =') throw new Error('invalid TOML fixture');",
            "  return {};",
            "};",
            "exports.stringify = function stringify() { return '[cli]\\ninstaller = \\\"npm\\\"\\n'; };",
        ].join('\n'),
    );
    if (withPlatformPackage) {
        fs.mkdirSync(path.join(platformPackage, 'bin'), { recursive: true });
        fs.writeFileSync(
            path.join(platformPackage, 'package.json'),
            JSON.stringify({ name: `@atelier/atelier-${process.platform}-${process.arch}`, version: '0.0.0' }),
        );
        if (withBinary) {
            const executable = process.platform === 'win32' ? 'atelier.exe' : 'atelier';
            fs.writeFileSync(path.join(platformPackage, 'bin', executable), 'fixture-binary');
        }
    }
    if (config !== undefined) {
        const configDir = path.join(home, '.atelier');
        fs.mkdirSync(configDir, { recursive: true });
        fs.writeFileSync(path.join(configDir, 'config.toml'), config);
    }

    const postinstallPath = path.join(
        repoRoot,
        'crates/codegen/atelier-pager/npm/atelier/bin/postinstall.js',
    );
    const result = childProcess.spawnSync(process.execPath, [postinstallPath], {
        encoding: 'utf8',
        env: {
            ...process.env,
            HOME: home,
            USERPROFILE: home,
            NODE_PATH: [nodeModules, process.env.NODE_PATH].filter(Boolean).join(path.delimiter),
        },
    });
    return { root, home, result };
}

{
    const fixture = runPostinstallFixture({ withPlatformPackage: false, withBinary: false });
    try {
        assert.notStrictEqual(fixture.result.status, 0, 'missing platform package must fail postinstall');
    } finally {
        fs.rmSync(fixture.root, { recursive: true, force: true });
    }
}

{
    const fixture = runPostinstallFixture({ withBinary: false });
    try {
        assert.notStrictEqual(fixture.result.status, 0, 'missing packaged binary must fail postinstall');
    } finally {
        fs.rmSync(fixture.root, { recursive: true, force: true });
    }
}

{
    const invalidConfig = '[cli\ninvalid =';
    const fixture = runPostinstallFixture({ withBinary: true, config: invalidConfig });
    try {
        const configPath = path.join(fixture.home, '.atelier', 'config.toml');
        assert.notStrictEqual(fixture.result.status, 0, 'invalid existing config must fail postinstall');
        assert.strictEqual(
            fs.readFileSync(configPath, 'utf8'),
            invalidConfig,
            'invalid existing config must remain byte-for-byte unchanged',
        );
        assert.match(fixture.result.stderr, /failed to parse existing config/i);
        const executable = process.platform === 'win32' ? 'atelier.exe' : 'atelier';
        assert.ok(
            !fs.existsSync(path.join(fixture.home, '.atelier', 'bin', executable)),
            'a config parse failure must happen before installing the binary',
        );
    } finally {
        fs.rmSync(fixture.root, { recursive: true, force: true });
    }
}

{
    const fixture = runPostinstallFixture({ withBinary: true });
    try {
        assert.strictEqual(fixture.result.status, 0, fixture.result.stderr);
        const executable = process.platform === 'win32' ? 'atelier.exe' : 'atelier';
        assert.strictEqual(
            fs.readFileSync(path.join(fixture.home, '.atelier', 'bin', executable), 'utf8'),
            'fixture-binary',
        );
        assert.match(
            fs.readFileSync(path.join(fixture.home, '.atelier', 'config.toml'), 'utf8'),
            /installer = "npm"/,
        );
    } finally {
        fs.rmSync(fixture.root, { recursive: true, force: true });
    }
}

console.log('release installation contract tests passed');
