#!/usr/bin/env node

import inquirer from 'inquirer';
import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';
import { readdirSync, statSync } from 'fs';
import yaml from 'js-yaml';
import os from 'os';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const TOOL_CONFIG = {
    cline: {
        ruleGlob: 'cline-rulestore-rule.md',
        ruleDir: 'cline',
        targetSubdir: '.clinerules',
    },
    roo: {
        ruleGlob: 'roo-rulestore-rule.md',
        ruleDir: 'roo',
        targetSubdir: '.roo/rules',
    },
    cursor: {
        ruleGlob: 'cursor-rulestore-rule.md',
        ruleDir: 'cursor',
        targetSubdir: '.cursor/rules',
    },
    claude: {
        ruleGlob: 'cursor-rulestore-rule.md',
        ruleDir: 'cursor',
        targetSubdir: '.claude/rules',
    },
    'claude-code-4.5': {
        ruleDir: 'packages',
        targetSubdir: '.claude',
        usePackagesStructure: true,
        externalDepTypes: ['claude-plugins', 'npx-skills', 'agent-skills'],
        packageMappings: {
            'skills': 'skills',
            'agents': 'agents',
            'utilities/utils': 'utils',
            'utilities/hooks': 'hooks',
            'utilities/output-styles': 'output-styles',
            'utilities/reflections': 'reflections'
        },
        // Tool-specific files still come from claude-code-4.5/
        toolSpecificFiles: [
            'claude-code-4.5/CLAUDE.md',
            'claude-code-4.5/settings.json'
        ],
        excludeFiles: ['settings.local.json'],
        templateSubstitutions: {
            '**/*.md': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            },
            '**/*.sh': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            },
            '**/*.py': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            },
            '**/*.js': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            },
            '**/*.ts': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            },
            '**/*.json': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            },
            '**/*.yaml': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            },
            '**/*.yml': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            },
            '**/*.toml': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            }
        }
    },
    codex: {
        ruleDir: 'codex',
        targetSubdir: '.codex',
        usePackagesStructure: true,
        forceHomeInstall: true,
        copyClaudeMd: false,
        copySettings: false,
        externalDepTypes: ['npx-skills', 'codex-skills', 'agent-skills'],
        packageMappings: {
            'skills': 'skills',
            'utilities/reflections': 'reflections'
        },
        projectRootCopies: ['AGENTS.md'],
        toolSpecificFiles: ['codex/config.toml'],
        templateSubstitutions: {
            '**/*.md': {
                'TOOL_DIR': '.codex',
                'HOME_TOOL_DIR': '~/.codex'
            },
            '**/*.sh': {
                'TOOL_DIR': '.codex',
                'HOME_TOOL_DIR': '~/.codex'
            },
            '**/*.py': {
                'TOOL_DIR': '.codex',
                'HOME_TOOL_DIR': '~/.codex'
            },
            '**/*.js': {
                'TOOL_DIR': '.codex',
                'HOME_TOOL_DIR': '~/.codex'
            },
            '**/*.ts': {
                'TOOL_DIR': '.codex',
                'HOME_TOOL_DIR': '~/.codex'
            },
            '**/*.json': {
                'TOOL_DIR': '.codex',
                'HOME_TOOL_DIR': '~/.codex'
            },
            '**/*.yaml': {
                'TOOL_DIR': '.codex',
                'HOME_TOOL_DIR': '~/.codex'
            },
            '**/*.yml': {
                'TOOL_DIR': '.codex',
                'HOME_TOOL_DIR': '~/.codex'
            },
            '**/*.toml': {
                'TOOL_DIR': '.codex',
                'HOME_TOOL_DIR': '~/.codex'
            }
        }
    },
    copilot: {
        ruleDir: 'copilot',
        targetSubdir: '.copilot',
        usePackagesStructure: true,
        forceHomeInstall: true,
        copyClaudeMd: false,
        copySettings: false,
        externalDepTypes: ['npx-skills', 'copilot-skills', 'agent-skills'],
        packageMappings: {
            'skills': 'skills',
            'utilities/reflections': 'reflections'
        },
        projectRootCopies: ['AGENTS.md'],
        templateSubstitutions: {
            '**/*.md': {
                'TOOL_DIR': '.copilot',
                'HOME_TOOL_DIR': '~/.copilot'
            },
            '**/*.sh': {
                'TOOL_DIR': '.copilot',
                'HOME_TOOL_DIR': '~/.copilot'
            },
            '**/*.py': {
                'TOOL_DIR': '.copilot',
                'HOME_TOOL_DIR': '~/.copilot'
            },
            '**/*.js': {
                'TOOL_DIR': '.copilot',
                'HOME_TOOL_DIR': '~/.copilot'
            },
            '**/*.ts': {
                'TOOL_DIR': '.copilot',
                'HOME_TOOL_DIR': '~/.copilot'
            },
            '**/*.json': {
                'TOOL_DIR': '.copilot',
                'HOME_TOOL_DIR': '~/.copilot'
            },
            '**/*.yaml': {
                'TOOL_DIR': '.copilot',
                'HOME_TOOL_DIR': '~/.copilot'
            },
            '**/*.yml': {
                'TOOL_DIR': '.copilot',
                'HOME_TOOL_DIR': '~/.copilot'
            },
            '**/*.toml': {
                'TOOL_DIR': '.copilot',
                'HOME_TOOL_DIR': '~/.copilot'
            }
        }
    },
    'packages': {
        ruleDir: 'packages',
        targetSubdir: '.claude',
        copyEntireFolder: true,
        usePackagesStructure: true,
        packageMappings: {
            'skills': 'skills',
            'agents': 'agents',
            'utilities/utils': 'utils',
            'utilities/hooks': 'hooks',
            'utilities/output-styles': 'output-styles',
            'utilities/reflections': 'reflections'
        },
        excludeFiles: [],
        templateSubstitutions: {
            '**/*.md': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            },
            '**/*.sh': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            },
            '**/*.py': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            },
            '**/*.js': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            },
            '**/*.ts': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            },
            '**/*.json': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            },
            '**/*.yaml': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            },
            '**/*.yml': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            },
            '**/*.toml': {
                'TOOL_DIR': '.claude',
                'HOME_TOOL_DIR': '~/.claude'
            }
        }
    },
    gemini: {
        ruleGlob: 'GEMINI.md',
        ruleDir: 'gemini',
        targetSubdir: '.gemini',
        usePackagesStructure: true,
        forceHomeInstall: true,
        copyClaudeMd: false,
        copySettings: false,
        externalDepTypes: ['npx-skills', 'agent-skills'],
        packageMappings: {
            'skills': 'skills',
            'agents': 'agents',
            'utilities/utils': 'utils',
            'utilities/hooks': 'hooks',
            'utilities/output-styles': 'output-styles',
            'utilities/reflections': 'reflections'
        },
        toolSpecificFiles: ['gemini/GEMINI.md', 'gemini/settings.json'],
        excludeFiles: ['settings.local.json'],
        templateSubstitutions: {
            '**/*.md': {
                'TOOL_DIR': '.gemini',
                'HOME_TOOL_DIR': '~/.gemini'
            },
            '**/*.sh': {
                'TOOL_DIR': '.gemini',
                'HOME_TOOL_DIR': '~/.gemini'
            },
            '**/*.py': {
                'TOOL_DIR': '.gemini',
                'HOME_TOOL_DIR': '~/.gemini'
            },
            '**/*.js': {
                'TOOL_DIR': '.gemini',
                'HOME_TOOL_DIR': '~/.gemini'
            },
            '**/*.ts': {
                'TOOL_DIR': '.gemini',
                'HOME_TOOL_DIR': '~/.gemini'
            },
            '**/*.json': {
                'TOOL_DIR': '.gemini',
                'HOME_TOOL_DIR': '~/.gemini'
            },
            '**/*.yaml': {
                'TOOL_DIR': '.gemini',
                'HOME_TOOL_DIR': '~/.gemini'
            },
            '**/*.yml': {
                'TOOL_DIR': '.gemini',
                'HOME_TOOL_DIR': '~/.gemini'
            },
            '**/*.toml': {
                'TOOL_DIR': '.gemini',
                'HOME_TOOL_DIR': '~/.gemini'
            }
        }
    },
    amazonq: {
        ruleGlob: 'q-rulestore-rule.md',
        ruleDir: 'amazonq',
        targetSubdir: '.amazonq/rules',
        mcpFile: 'amazonq/mcp.json',
        mcpTarget: '.amazonq/mcp.json',
        usePackagesStructure: true,
        packageMappings: {
            'skills': 'skills',
            'agents': 'agents',
            'utilities/hooks': 'hooks',
            'utilities/output-styles': 'output-styles',
            'utilities/reflections': 'reflections'
        },
        toolSpecificFiles: ['amazonq/AmazonQ.md'],
        specialCopies: [
            {
                source: 'amazonq/AmazonQ.md',
                dest: '.amazonq/rules/AmazonQ.md'
            }
        ],
        linkedFiles: [
            {
                source: 'amazonq/AmazonQ.md',
                linkName: 'AmazonQ.md'
            }
        ],
        excludeFiles: ['settings.local.json'],
        templateSubstitutions: {
            '**/*.md': {
                'TOOL_DIR': '.amazonq',
                'HOME_TOOL_DIR': '.amazonq'
            },
            '**/*.sh': {
                'TOOL_DIR': '.amazonq',
                'HOME_TOOL_DIR': '.amazonq'
            },
            '**/*.py': {
                'TOOL_DIR': '.amazonq',
                'HOME_TOOL_DIR': '.amazonq'
            }
        }
    },
    clawdhub: {
        ruleDir: 'clawdhub-skills',
        targetSubdir: 'skills',
        useClawdhubStructure: true,
        excludeFiles: ['_meta.json'],  // Don't copy auto-generated ClawdHub metadata
        templateSubstitutions: {
            '**/*.md': {
                'TOOL_DIR': '.clawdbot',
                'HOME_TOOL_DIR': '~/.clawdbot'
            }
        }
    },
};

// Maps internal tool IDs to canonical names used in applies-to fields of the manifest
const TOOL_CANONICAL_NAMES = {
    'claude-code-4.5': 'claude',
};

const GENERAL_RULES_DIR = path.join(__dirname, 'general-rules');
const ALWAYS_COPY_RULES = [
    'rule-interpreter-rule.md',
    'rulestyle-rule.md',
];

// Discover available packages for interactive selection
function discoverPackages(packagesDir) {
    const packages = {
        skills: [],
        agentCategories: [],
        utilities: [],
        externalPlugins: [],
        npxSkills: []
    };

    // Read external dependencies manifest
    const manifestPath = path.join(path.dirname(packagesDir), 'external-dependencies.yaml');
    if (fs.existsSync(manifestPath)) {
        try {
            const manifestContent = fs.readFileSync(manifestPath, 'utf8');
            const manifest = yaml.load(manifestContent);

            // Claude plugins
            if (manifest['claude-plugins'] && Array.isArray(manifest['claude-plugins'])) {
                for (const plugin of manifest['claude-plugins']) {
                    packages.externalPlugins.push({
                        name: plugin.name,
                        marketplace: plugin.marketplace,
                        version: plugin.version || 'latest',
                        purpose: plugin.purpose || plugin.name,
                        install: plugin.install
                    });
                }
            }

            // npx skills
            if (manifest['npx-skills'] && Array.isArray(manifest['npx-skills']) && manifest['npx-skills'].length > 0) {
                for (const skill of manifest['npx-skills']) {
                    packages.npxSkills.push({
                        name: skill.name,
                        repo: skill.repo,
                        purpose: skill.purpose || skill.name,
                        install: skill.install
                    });
                }
            }
        } catch (err) {
            // Silently ignore manifest parse errors
        }
    }

    // Discover skills (each skill is individually selectable)
    const skillsDir = path.join(packagesDir, 'skills');
    if (fs.existsSync(skillsDir)) {
        const skillDirs = readdirSync(skillsDir).filter(f =>
            statSync(path.join(skillsDir, f)).isDirectory()
        );
        for (const skill of skillDirs) {
            const skillPath = path.join(skillsDir, skill);
            const skillMd = path.join(skillPath, 'SKILL.md');
            let description = skill;
            if (fs.existsSync(skillMd)) {
                const content = fs.readFileSync(skillMd, 'utf8');
                const descMatch = content.match(/description:\s*["']?([^"'\n]+)/);
                if (descMatch) description = descMatch[1].trim();
            }
            packages.skills.push({ name: skill, path: `skills/${skill}`, description });
        }
    }

    // Discover agent categories
    const agentsDir = path.join(packagesDir, 'agents');
    if (fs.existsSync(agentsDir)) {
        const categories = readdirSync(agentsDir).filter(f =>
            statSync(path.join(agentsDir, f)).isDirectory()
        );
        for (const cat of categories) {
            const catPath = path.join(agentsDir, cat);
            const agentFiles = readdirSync(catPath).filter(f => f.endsWith('.md'));
            packages.agentCategories.push({
                name: cat,
                path: `agents/${cat}`,
                count: agentFiles.length,
                agents: agentFiles.map(f => f.replace('.md', ''))
            });
        }
    }

    // Discover other utilities
    const utilityDirs = ['hooks', 'output-styles', 'reflections'];
    for (const util of utilityDirs) {
        const utilDir = path.join(packagesDir, 'utilities', util);
        if (fs.existsSync(utilDir)) {
            const files = readdirSync(utilDir);
            packages.utilities.push({ name: util, path: `utilities/${util}`, count: files.length });
        }
    }

    return packages;
}

// Build choices for interactive selection
function buildPackageChoices(packages) {
    const choices = [];

    // Skills section
    if (packages.skills.length > 0) {
        choices.push(new inquirer.Separator('─── Skills ───'));
        for (const skill of packages.skills) {
            choices.push({
                name: `${skill.name} - ${skill.description}`,
                value: { type: 'skill', name: skill.name, path: skill.path },
                checked: true
            });
        }
    }

    // Agent categories section
    if (packages.agentCategories.length > 0) {
        choices.push(new inquirer.Separator('─── Agents ───'));
        for (const cat of packages.agentCategories) {
            choices.push({
                name: `${cat.name} (${cat.count} agents: ${cat.agents.slice(0, 3).join(', ')}${cat.count > 3 ? '...' : ''})`,
                value: { type: 'agents', name: cat.name, path: cat.path },
                checked: true
            });
        }
    }

    // Utilities section
    if (packages.utilities.length > 0) {
        choices.push(new inquirer.Separator('─── Utilities ───'));
        for (const util of packages.utilities) {
            choices.push({
                name: `${util.name} (${util.count} files)`,
                value: { type: 'utility', name: util.name, path: util.path },
                checked: true
            });
        }
    }

    // External Claude plugins section
    if (packages.externalPlugins.length > 0) {
        choices.push(new inquirer.Separator('─── External Plugins (claude plugin) ───'));
        for (const plugin of packages.externalPlugins) {
            choices.push({
                name: `${plugin.name} - ${plugin.purpose} [${plugin.marketplace}]`,
                value: { type: 'external-plugin', name: plugin.name, marketplace: plugin.marketplace, install: plugin.install },
                checked: false  // External deps unchecked by default
            });
        }
    }

    // npx skills section
    if (packages.npxSkills.length > 0) {
        choices.push(new inquirer.Separator('─── npx Skills ───'));
        for (const skill of packages.npxSkills) {
            choices.push({
                name: `${skill.name} - ${skill.purpose} [${skill.repo}]`,
                value: { type: 'npx-skill', name: skill.name, repo: skill.repo, install: skill.install },
                checked: false  // External deps unchecked by default
            });
        }
    }

    return choices;
}

function getGeneralRuleFiles() {
    return readdirSync(GENERAL_RULES_DIR)
        .filter(f => f.endsWith('.md'))
        .filter(f => !ALWAYS_COPY_RULES.includes(f));
}

function parseFrontMatter(filePath) {
    const content = fs.readFileSync(filePath, 'utf8');
    const match = content.match(/^---([\s\S]*?)---/);
    if (!match) return {};
    try {
        return yaml.load(match[1]);
    } catch {
        return {};
    }
}

function isExternalSourcedSkill(skillDir) {
    const skillMd = path.join(skillDir, 'SKILL.md');
    if (!fs.existsSync(skillMd)) return false;
    const content = fs.readFileSync(skillMd, 'utf8');
    const match = content.match(/^---([\s\S]*?)---/);
    if (!match) return false;
    try {
        const fm = yaml.load(match[1]);
        return !!(fm && fm['external-source']);
    } catch { return false; }
}

function validateSkillFrontmatter(skillsDir) {
    const errors = [];

    function walk(dir) {
        const items = readdirSync(dir);
        for (const item of items) {
            const fullPath = path.join(dir, item);
            const isDirectory = statSync(fullPath).isDirectory();
            if (isDirectory) {
                walk(fullPath);
                continue;
            }
            if (item !== 'SKILL.md') {
                continue;
            }

            const content = fs.readFileSync(fullPath, 'utf8');
            const match = content.match(/^---([\s\S]*?)---/);
            if (!match) {
                errors.push(`${fullPath}: missing YAML frontmatter`);
                continue;
            }
            try {
                yaml.load(match[1]);
            } catch (err) {
                errors.push(`${fullPath}: ${err.message}`);
            }
        }
    }

    if (fs.existsSync(skillsDir)) {
        walk(skillsDir);
    }

    if (errors.length > 0) {
        const errorMessage = `Invalid SKILL.md YAML frontmatter:\n- ${errors.join('\n- ')}`;
        throw new Error(errorMessage);
    }
}

function showProgress(message, isComplete = false) {
    const greenCheck = '\x1b[32m✓\x1b[0m';
    
    if (isComplete) {
        console.log(`${greenCheck} ${message}`);
    } else {
        process.stdout.write(`⠋ ${message}...`);
    }
}

function completeProgress(message) {
    process.stdout.write('\r\x1b[K'); // Clear the line
    showProgress(message, true);
}

function substituteTemplate(content, substitutions) {
    let result = content;
    for (const [placeholder, value] of Object.entries(substitutions)) {
        const regex = new RegExp(`{{${placeholder}}}`, 'g');
        result = result.replace(regex, value);
    }
    return result;
}

function getEffectiveExcludeFiles(tool, config) {
    const excludeFiles = [...(config.excludeFiles || [])];

    // Exclude agents folder for all tools except claude-code-4.5
    if (tool !== 'claude-code-4.5') {
        excludeFiles.push('agents');
    }

    return excludeFiles;
}

function copyDirectoryRecursive(source, destination, excludeFiles = [], templateSubstitutions = {}) {
    const files = [];

    function shouldSkipItem(item, relativePath, isDirectory) {
        // Skip node_modules and logs directories
        if (isDirectory && (item === 'node_modules' || item === 'logs')) {
            return true;
        }

        // Skip build artifacts and lock files
        if (!isDirectory) {
            // Skip Bun build temp files
            if (item.startsWith('.') && item.includes('.bun-build')) {
                return true;
            }
            // Skip lock files in bin directories
            if (relativePath.includes('/bin/') && (item === 'bun.lockb' || item === 'bun.lock')) {
                return true;
            }
        }

        // Check explicit excludes
        return excludeFiles.some(excludeFile =>
            relativePath === excludeFile || item === excludeFile
        );
    }

    function getAllFiles(dir, basePath = '') {
        const items = readdirSync(dir);
        for (const item of items) {
            const fullPath = path.join(dir, item);
            const relativePath = path.join(basePath, item);
            const isDirectory = statSync(fullPath).isDirectory();

            if (shouldSkipItem(item, relativePath, isDirectory)) {
                continue;
            }

            if (isDirectory) {
                getAllFiles(fullPath, relativePath);
            } else {
                files.push({ source: fullPath, dest: path.join(destination, relativePath), fileName: item, relativePath: relativePath });
            }
        }
    }
    
    getAllFiles(source);
    
    for (const file of files) {
        const destDir = path.dirname(file.dest);
        fs.mkdirSync(destDir, { recursive: true });
        
        // Check if this file needs template substitution
        // Support both exact filename and relative path matching
        let substitutions = templateSubstitutions[file.fileName] || 
                            templateSubstitutions[file.relativePath] || 
                            (file.relativePath.endsWith('.md') ? templateSubstitutions['**/*.md'] : null);

        if (!substitutions) {
            for (const [pattern, patternSubs] of Object.entries(templateSubstitutions)) {
                if (pattern.startsWith('**/*.') && file.relativePath.endsWith(pattern.slice(4))) {
                    substitutions = patternSubs;
                    break;
                }
            }
        }
        
        if (substitutions) {
            let content = fs.readFileSync(file.source, 'utf8');
            content = substituteTemplate(content, substitutions);
            fs.writeFileSync(file.dest, content);
        } else {
            fs.copyFileSync(file.source, file.dest);
        }
    }
    
    return files.length;
}

async function handleSharedContentCopy(tool, config, targetFolder) {
    if (!targetFolder) {
        const answers = await inquirer.prompt([
            {
                type: 'input',
                name: 'targetFolder',
                message: 'Enter the target project folder:',
                validate: (input) => !!input.trim() || 'Folder name required',
            },
        ]);
        targetFolder = answers.targetFolder;
    }

    showProgress('Creating target directory');
    if (!fs.existsSync(targetFolder)) {
        fs.mkdirSync(targetFolder, { recursive: true });
        completeProgress(`Created folder: ${targetFolder}`);
    } else {
        completeProgress(`Using existing folder: ${targetFolder}`);
    }

    const destDir = path.join(targetFolder, config.targetSubdir);
    fs.mkdirSync(destDir, { recursive: true });

    // Copy shared content from claude-code
    showProgress('Copying shared content from claude-code');
    const sharedSourceDir = path.join(__dirname, config.sharedContentDir);
    
    const excludeFiles = getEffectiveExcludeFiles(tool, config);
    const sharedFilesCopied = copyDirectoryRecursive(sharedSourceDir, destDir, excludeFiles, config.templateSubstitutions || {});
    completeProgress(`Copied ${sharedFilesCopied} shared files`);

    // Copy tool-specific files
    showProgress('Copying tool-specific files');
    const toolSpecificPath = path.join(__dirname, config.ruleDir, config.ruleGlob);
    if (fs.existsSync(toolSpecificPath)) {
        // For GEMINI.md, copy CLAUDE.md and apply substitutions
        if (config.ruleGlob === 'GEMINI.md') {
            const claudePath = path.join(__dirname, 'claude-code-4.5', 'CLAUDE.md');
            let content = fs.readFileSync(claudePath, 'utf8');
            if (config.templateSubstitutions && config.templateSubstitutions['GEMINI.md']) {
                content = substituteTemplate(content, config.templateSubstitutions['GEMINI.md']);
            }
            fs.writeFileSync(path.join(destDir, config.ruleGlob), content);
        } else {
            fs.copyFileSync(toolSpecificPath, path.join(destDir, config.ruleGlob));
        }
        completeProgress('Copied tool-specific files');
    }

    // Copy always copy rules
    showProgress('Copying core rules');
    for (const rule of ALWAYS_COPY_RULES) {
        fs.copyFileSync(path.join(GENERAL_RULES_DIR, rule), path.join(destDir, rule));
    }
    completeProgress('Copied core rules');

    // Copy shared content to specific target if specified
    if (config.sharedContentTarget) {
        const targetDir = path.join(targetFolder, config.sharedContentTarget);
        showProgress(`Copying shared content to ${config.sharedContentTarget}`);
        const sharedSourceDir = path.join(__dirname, config.sharedContentDir);
        fs.mkdirSync(targetDir, { recursive: true });
        const excludeFiles = getEffectiveExcludeFiles(tool, config);
        const sharedFilesCopied = copyDirectoryRecursive(sharedSourceDir, targetDir, excludeFiles, config.templateSubstitutions || {});
        completeProgress(`Copied ${sharedFilesCopied} shared files to ${config.sharedContentTarget}`);
    }

    // Copy settings file to project directory if specified
    if (config.settingsFile) {
        showProgress('Copying settings file');
        const sourcePath = path.join(__dirname, config.settingsFile);
        const destPath = path.join(destDir, 'settings.json');
        
        if (fs.existsSync(sourcePath)) {
            fs.copyFileSync(sourcePath, destPath);
            completeProgress('Copied settings file');
        }
    }

    // Copy MCP file to specified target if specified
    if (config.mcpFile && config.mcpTarget) {
        showProgress('Copying MCP configuration');
        const sourcePath = path.join(__dirname, config.mcpFile);
        const destPath = path.join(targetFolder, config.mcpTarget);
        
        if (fs.existsSync(sourcePath)) {
            fs.mkdirSync(path.dirname(destPath), { recursive: true });
            fs.copyFileSync(sourcePath, destPath);
            completeProgress(`Copied MCP config to ${config.mcpTarget}`);
        }
    }

    // Perform special file copies
    if (config.specialCopies) {
        showProgress('Performing special file copies');
        let specialFilesCopied = 0;
        for (const copy of config.specialCopies) {
            const sourcePath = path.join(__dirname, copy.source);
            const destPath = path.join(targetFolder, copy.dest);
            const fileName = path.basename(copy.source);

            if (fs.existsSync(sourcePath)) {
                fs.mkdirSync(path.dirname(destPath), { recursive: true });

                if (config.templateSubstitutions && config.templateSubstitutions[fileName]) {
                    let content = fs.readFileSync(sourcePath, 'utf8');
                    content = substituteTemplate(content, config.templateSubstitutions[fileName]);
                    fs.writeFileSync(destPath, content);
                } else {
                    fs.copyFileSync(sourcePath, destPath);
                }
                specialFilesCopied++;
            }
        }
        completeProgress(`Copied ${specialFilesCopied} special files`);
    }

    

    // Create linked files if they exist
    if (config.linkedFiles) {
        showProgress('Creating linked files');
        let linkedFilesCreated = 0;
        for (const link of config.linkedFiles) {
            const linkPath = path.join(targetFolder, link.linkName);
            const sourcePath = path.join(config.targetSubdir, link.source.split('/').pop());
            fs.writeFileSync(linkPath, `@${sourcePath}`);
            linkedFilesCreated++;
        }
        completeProgress(`Created ${linkedFilesCreated} linked files`);
    }

    console.log(`\n\x1b[32m🎉 ${tool} setup complete!\x1b[0m`);
    console.log(`Files copied to: ${destDir}`);
}

async function handlePackagesStructureCopy(tool, config, overrideHomeDir = null, targetFolder = null, isNonInteractive = false, specifiedPackages = null) {
    let destDir;
    let displayPath;

    const shouldUseHome = !targetFolder || config.forceHomeInstall;
    if (!shouldUseHome) {
        destDir = path.join(targetFolder, config.targetSubdir);
        displayPath = path.join(targetFolder, config.targetSubdir);
    } else {
        const homeDir = overrideHomeDir || os.homedir();
        destDir = path.join(homeDir, config.targetSubdir);
        displayPath = `~/${config.targetSubdir}`;
    }

    const packagesDir = path.join(__dirname, 'packages');
    let totalFilesCopied = 0;

    // Package selection for project installations (not home directory)
    let selectedPackagePaths = null;

    // If packages specified via CLI, use those
    if (!shouldUseHome && specifiedPackages) {
        selectedPackagePaths = new Set(specifiedPackages);
        console.log(`\n📦 Installing specified packages: ${specifiedPackages.join(', ')}\n`);
    }
    // Interactive package selection for project installations
    else if (!shouldUseHome && !isNonInteractive) {
        const availablePackages = discoverPackages(packagesDir);
        const choices = buildPackageChoices(availablePackages);

        console.log('\n📦 Select packages to install in your project:\n');
        const { selectedPackages } = await inquirer.prompt([
            {
                type: 'checkbox',
                name: 'selectedPackages',
                message: 'Use space to toggle, enter to confirm:',
                choices: choices,
                pageSize: 20
            }
        ]);

        // Build list of selected paths and external deps
        selectedPackagePaths = new Set();
        const selectedExternalDeps = [];

        for (const pkg of selectedPackages) {
            if (pkg.type === 'skill') {
                selectedPackagePaths.add(pkg.path);
            } else if (pkg.type === 'agents') {
                selectedPackagePaths.add(pkg.path);
            } else if (pkg.type === 'utility') {
                selectedPackagePaths.add(pkg.path);
            } else if (pkg.type === 'external-plugin' || pkg.type === 'npx-skill') {
                selectedExternalDeps.push(pkg);
            }
        }

        // Generate setup-external.sh if external deps were selected
        if (selectedExternalDeps.length > 0) {
            const scriptLines = [
                '#!/bin/bash',
                '# External dependencies for this project',
                '# Generated by create-rule.js',
                `# Run from project root: bash ${config.targetSubdir}/setup-external.sh`,
                '#',
                '# Plugins are installed at PROJECT scope (not user scope)',
                '# This avoids conflicts with user-level plugin installations',
                '',
                'set -e',
                '',
                '# Ensure we are in the project directory',
                'SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"',
                'PROJECT_DIR="$(dirname "$SCRIPT_DIR")"',
                'cd "$PROJECT_DIR"',
                ''
            ];

            const plugins = selectedExternalDeps.filter(d => d.type === 'external-plugin');
            const npxSkills = selectedExternalDeps.filter(d => d.type === 'npx-skill');

            if (plugins.length > 0) {
                scriptLines.push('echo "Installing Claude plugins (project scope)..."');
                scriptLines.push('');

                // Collect unique marketplaces
                const marketplaces = new Set();
                for (const plugin of plugins) {
                    if (plugin.marketplace) {
                        marketplaces.add(plugin.marketplace);
                    }
                }

                // Add marketplaces first (skip if already exists)
                for (const marketplace of marketplaces) {
                    scriptLines.push(`claude plugin marketplace add ${marketplace} 2>/dev/null || true`);
                }
                scriptLines.push('');

                // Install plugins at project scope
                for (const plugin of plugins) {
                    scriptLines.push(`claude plugin install ${plugin.name} --scope project`);
                }
                scriptLines.push('');
            }

            if (npxSkills.length > 0) {
                scriptLines.push('echo "Installing npx skills..."');
                scriptLines.push('');
                for (const skill of npxSkills) {
                    scriptLines.push(skill.install || `npx skills add ${skill.repo}`);
                }
                scriptLines.push('');
            }

            scriptLines.push('echo "✓ External dependencies installed!"');

            // Store for later writing (after destDir is created)
            config._externalDepsScript = scriptLines.join('\n');
            config._externalDepsCount = selectedExternalDeps.length;
        }

        if (selectedPackagePaths.size === 0 && selectedExternalDeps.length === 0) {
            console.log('\n⚠️  No packages selected. Only copying core files.\n');
        }
    }
    // Generate setup-external.sh for home directory installs (codex, copilot, claude-code-4.5)
    else if (shouldUseHome && config.externalDepTypes && config.externalDepTypes.length > 0) {
        const manifestPath = path.join(path.dirname(packagesDir), 'external-dependencies.yaml');
        if (fs.existsSync(manifestPath)) {
            try {
                const manifestContent = fs.readFileSync(manifestPath, 'utf8');
                const manifest = yaml.load(manifestContent);

                const applicableDeps = [];
                for (const depType of config.externalDepTypes) {
                    if (manifest[depType] && Array.isArray(manifest[depType])) {
                        for (const dep of manifest[depType]) {
                            applicableDeps.push({ ...dep, _depType: depType });
                        }
                    }
                }

                if (applicableDeps.length > 0) {
                    const toolName = tool;
                    const scriptLines = [
                        '#!/bin/bash',
                        `# External marketplace skills and plugins for ${toolName}`,
                        '# Generated by create-rule.js',
                        `# Run: bash ~/${config.targetSubdir}/setup-external.sh`,
                        '',
                        'set -e',
                        ''
                    ];

                    const claudePlugins = applicableDeps.filter(d => d._depType === 'claude-plugins');
                    const npxSkills = applicableDeps.filter(d => d._depType === 'npx-skills');
                    const agentSkills = applicableDeps.filter(d => d._depType === 'agent-skills');
                    const otherSkills = applicableDeps.filter(d => d._depType !== 'claude-plugins' && d._depType !== 'npx-skills' && d._depType !== 'agent-skills');

                    if (claudePlugins.length > 0) {
                        scriptLines.push('echo "Installing Claude plugins..."');
                        scriptLines.push('');
                        const marketplaces = new Set(claudePlugins.map(p => p.marketplace).filter(Boolean));
                        for (const m of marketplaces) {
                            scriptLines.push(`claude plugin marketplace add ${m} 2>/dev/null || true`);
                        }
                        scriptLines.push('');
                        for (const p of claudePlugins) {
                            scriptLines.push(`claude plugin install ${p.name}`);
                        }
                        scriptLines.push('');
                    }

                    if (npxSkills.length > 0) {
                        scriptLines.push('echo "Installing npx agent skills..."');
                        scriptLines.push('');
                        for (const s of npxSkills) {
                            scriptLines.push(s.install || `npx add-skill ${s.repo}`);
                        }
                        scriptLines.push('');
                    }

                    // Filter agent-skills by applies-to field
                    // Use canonical name so e.g. 'claude-code-4.5' matches 'claude' in applies-to
                    const canonicalToolName = TOOL_CANONICAL_NAMES[toolName] || toolName;
                    const relevantAgentSkills = agentSkills.filter(skill => {
                        if (!skill['applies-to']) return true;
                        return skill['applies-to'].includes(toolName) || skill['applies-to'].includes(canonicalToolName);
                    });

                    if (relevantAgentSkills.length > 0) {
                        scriptLines.push('echo "Installing agent skills (git repos)..."');
                        scriptLines.push('');
                        for (const skill of relevantAgentSkills) {
                            const skillDir = `"\${HOME}/${config.targetSubdir}/skills/${skill.name}"`;
                            scriptLines.push(`# ${skill.name}${skill.purpose ? ' - ' + skill.purpose : ''}`);
                            scriptLines.push(`SKILL_DIR=${skillDir}`);
                            scriptLines.push(`if [ -d "\${SKILL_DIR}/.git" ]; then`);
                            scriptLines.push(`  echo "  Updating ${skill.name}..."`);
                            scriptLines.push(`  git -C "\${SKILL_DIR}" pull --ff-only`);
                            scriptLines.push(`else`);
                            scriptLines.push(`  echo "  Installing ${skill.name}..."`);
                            scriptLines.push(`  git clone ${skill.repo} "\${SKILL_DIR}"`);
                            scriptLines.push(`fi`);
                            scriptLines.push('');
                        }
                    }

                    if (otherSkills.length > 0) {
                        scriptLines.push('echo "Installing additional marketplace skills..."');
                        scriptLines.push('');
                        for (const s of otherSkills) {
                            if (s.install) scriptLines.push(s.install);
                        }
                        scriptLines.push('');
                    }

                    scriptLines.push('echo "✓ External dependencies installed!"');
                    config._externalDepsScript = scriptLines.join('\n');
                    config._externalDepsCount = applicableDeps.length;
                }
            } catch (err) {
                // Silently ignore manifest parse errors
            }
        }
    }

    showProgress(`Checking ${displayPath} directory`);
    if (!fs.existsSync(destDir)) {
        fs.mkdirSync(destDir, { recursive: true });
        completeProgress(`Created ${displayPath} directory`);
    } else {
        completeProgress(`Found ${displayPath} directory`);
    }

    if (config.validateSkillFrontmatter !== false && config.packageMappings && Object.prototype.hasOwnProperty.call(config.packageMappings, 'skills')) {
        showProgress('Validating SKILL.md frontmatter');
        try {
            validateSkillFrontmatter(path.join(packagesDir, 'skills'));
            completeProgress('Validated SKILL.md frontmatter');
        } catch (error) {
            completeProgress('SKILL.md frontmatter validation failed');
            throw error;
        }
    }

    // Copy using package mappings (filtered by selection if project install)
    for (const [source, target] of Object.entries(config.packageMappings)) {
        // If we have a selection, check if this source is selected
        if (selectedPackagePaths !== null) {
            // Check if this exact path or a parent path is selected
            const isSelected = [...selectedPackagePaths].some(sel => {
                // Exact match or source starts with selected path
                return source === sel || source.startsWith(sel + '/') || sel.startsWith(source + '/') || sel === source;
            });

            // Special handling for skills - copy individual skills
            if (source === 'skills') {
                const skillsSelected = [...selectedPackagePaths].filter(p => p.startsWith('skills/'));
                if (skillsSelected.length > 0) {
                    for (const skillPath of skillsSelected) {
                        const sourceDir = path.join(packagesDir, skillPath);
                        const skillName = skillPath.split('/')[1];
                        if (isExternalSourcedSkill(sourceDir)) {
                            continue; // Handled by setup-external.sh git clone
                        }
                        const targetDir = path.join(destDir, 'skills', skillName);
                        if (fs.existsSync(sourceDir)) {
                            showProgress(`Copying skill: ${skillName}`);
                            fs.mkdirSync(targetDir, { recursive: true });
                            const filesCopied = copyDirectoryRecursive(sourceDir, targetDir, config.excludeFiles || [], config.templateSubstitutions || {});
                            totalFilesCopied += filesCopied;
                            completeProgress(`Copied ${filesCopied} files from ${skillName}`);
                        }
                    }
                }
                continue; // Skip the default skills copy
            }

            // Special handling for agents - copy individual agent categories
            if (source === 'agents') {
                const agentsSelected = [...selectedPackagePaths].filter(p => p.startsWith('agents/'));
                if (agentsSelected.length > 0) {
                    for (const agentPath of agentsSelected) {
                        const sourceDir = path.join(packagesDir, agentPath);
                        const categoryName = agentPath.split('/')[1];
                        const targetDir = path.join(destDir, 'agents', categoryName);
                        if (fs.existsSync(sourceDir)) {
                            showProgress(`Copying agents: ${categoryName}`);
                            fs.mkdirSync(targetDir, { recursive: true });
                            const filesCopied = copyDirectoryRecursive(sourceDir, targetDir, config.excludeFiles || [], config.templateSubstitutions || {});
                            totalFilesCopied += filesCopied;
                            completeProgress(`Copied ${filesCopied} files from agents/${categoryName}`);
                        }
                    }
                }
                continue; // Skip the default agents copy
            }

            if (!isSelected) {
                continue; // Skip unselected packages
            }
        }

        const sourceDir = path.join(packagesDir, source);
        const targetDir = path.join(destDir, target);

        if (fs.existsSync(sourceDir)) {
            // For skills, copy individually so external-sourced skills can be skipped
            if (source === 'skills') {
                fs.mkdirSync(targetDir, { recursive: true });
                const skillDirs = readdirSync(sourceDir).filter(f =>
                    statSync(path.join(sourceDir, f)).isDirectory()
                );
                for (const skillName of skillDirs) {
                    const skillSrcDir = path.join(sourceDir, skillName);
                    if (isExternalSourcedSkill(skillSrcDir)) {
                        continue; // Handled by setup-external.sh git clone
                    }
                    showProgress(`Copying skill: ${skillName}`);
                    const skillDestDir = path.join(targetDir, skillName);
                    fs.mkdirSync(skillDestDir, { recursive: true });
                    const filesCopied = copyDirectoryRecursive(skillSrcDir, skillDestDir, config.excludeFiles || [], config.templateSubstitutions || {});
                    totalFilesCopied += filesCopied;
                    completeProgress(`Copied ${filesCopied} files from skills/${skillName}`);
                }
            } else {
                showProgress(`Copying ${source} to ${target}`);
                fs.mkdirSync(targetDir, { recursive: true });
                const filesCopied = copyDirectoryRecursive(sourceDir, targetDir, config.excludeFiles || [], config.templateSubstitutions || {});
                totalFilesCopied += filesCopied;
                completeProgress(`Copied ${filesCopied} files from ${source}`);
            }
        }
    }

    // Clean up stale commands and templates directories (deprecated - now skills)
    const staleCommandsDir = path.join(destDir, 'commands');
    const staleTemplatesDir = path.join(destDir, 'templates');
    const stalePromptsDir = path.join(destDir, 'prompts');
    for (const staleDir of [staleCommandsDir, staleTemplatesDir, stalePromptsDir]) {
        if (fs.existsSync(staleDir)) {
            fs.rmSync(staleDir, { recursive: true });
            console.log(`  Removed deprecated ${path.basename(staleDir)}/ directory`);
        }
    }

    // Copy CLAUDE.md from claude-code-4.5 if it exists
    // Skip for project folder installations to avoid overwriting project-specific CLAUDE.md
    if (config.copyClaudeMd !== false && shouldUseHome) {
        const claudeMdSource = path.join(__dirname, 'claude-code-4.5', 'CLAUDE.md');
        if (fs.existsSync(claudeMdSource)) {
            showProgress('Copying CLAUDE.md');
            let content = fs.readFileSync(claudeMdSource, 'utf8');
            if (config.templateSubstitutions && config.templateSubstitutions['**/*.md']) {
                content = substituteTemplate(content, config.templateSubstitutions['**/*.md']);
            }
            fs.writeFileSync(path.join(destDir, 'CLAUDE.md'), content);
            totalFilesCopied++;
            completeProgress('Copied CLAUDE.md');
        }
    } else if (!shouldUseHome) {
        console.log('\x1b[33m⚠\x1b[0m  Skipping CLAUDE.md (project folder - won\'t overwrite existing)');
    }

    // Copy settings.json from claude-code-4.5 if it exists
    // Skip for project folder installations to avoid overwriting project-specific settings
    if (config.copySettings !== false && shouldUseHome) {
        const settingsSource = path.join(__dirname, 'claude-code-4.5', 'settings.json');
        if (fs.existsSync(settingsSource)) {
            showProgress('Copying settings.json');
            fs.copyFileSync(settingsSource, path.join(destDir, 'settings.json'));
            totalFilesCopied++;
            completeProgress('Copied settings.json');
        }
    }

    if (config.toolSpecificFiles) {
        showProgress('Copying tool-specific files');
        let toolFilesCopied = 0;
        for (const toolFile of config.toolSpecificFiles) {
            const sourcePath = path.join(__dirname, toolFile);
            const fileName = path.basename(toolFile);
            const destPath = path.join(destDir, fileName);

            if (fs.existsSync(sourcePath)) {
                const substitutions = (config.templateSubstitutions || {})[fileName] ||
                    (fileName.endsWith('.md') ? (config.templateSubstitutions || {})['**/*.md'] : null);

                if (substitutions) {
                    let content = fs.readFileSync(sourcePath, 'utf8');
                    content = substituteTemplate(content, substitutions);
                    fs.writeFileSync(destPath, content);
                } else {
                    fs.copyFileSync(sourcePath, destPath);
                }
                toolFilesCopied++;
            }
        }
        completeProgress(`Copied ${toolFilesCopied} tool-specific files`);
    }

    if (config.projectRootCopies) {
        const rootTarget = targetFolder || (config.forceHomeInstall ? destDir : null);
        if (rootTarget) {
            if (!fs.existsSync(rootTarget)) {
                fs.mkdirSync(rootTarget, { recursive: true });
            }
        }
        showProgress('Copying project root files');
        let rootFilesCopied = 0;
        for (const fileName of config.projectRootCopies) {
            const sourcePath = path.join(__dirname, config.ruleDir, fileName);
            if (!fs.existsSync(sourcePath) || !rootTarget) {
                continue;
            }
            const destPath = path.join(rootTarget, fileName);

            // Ensure parent directory exists (supports subdirectory paths like .github/copilot-instructions.md)
            fs.mkdirSync(path.dirname(destPath), { recursive: true });

            const substitutions = (config.templateSubstitutions || {})[fileName] ||
                (fileName.endsWith('.md') ? (config.templateSubstitutions || {})['**/*.md'] : null);

            if (substitutions) {
                let content = fs.readFileSync(sourcePath, 'utf8');
                content = substituteTemplate(content, substitutions);
                fs.writeFileSync(destPath, content);
            } else {
                fs.copyFileSync(sourcePath, destPath);
            }
            rootFilesCopied++;
        }
        completeProgress(`Copied ${rootFilesCopied} project root files`);
    }

    // Write setup-external.sh if external deps were selected
    if (config._externalDepsScript) {
        const scriptPath = path.join(destDir, 'setup-external.sh');
        showProgress('Generating setup-external.sh');
        fs.writeFileSync(scriptPath, config._externalDepsScript);
        fs.chmodSync(scriptPath, 0o755);
        completeProgress(`Generated setup-external.sh (${config._externalDepsCount} external deps)`);
    }

    console.log(`\n\x1b[32m🎉 packages setup complete!\x1b[0m`);
    console.log(`Files copied to: ${destDir} (${totalFilesCopied} files)`);
    console.log(`\nPackages structure installed. Components available:`);
    console.log(`  - Skills: ${destDir}/skills/`);
    console.log(`  - Agents: ${destDir}/agents/`);
    console.log(`  - Hooks: ${destDir}/hooks/`);
    console.log(`  - Utils: ${destDir}/utils/`);

    // Show external deps instructions if script was generated
    if (config._externalDepsScript) {
        console.log(`\n\x1b[33m📦 External dependencies:\x1b[0m`);
        console.log(`  Run: bash ${destDir}/setup-external.sh`);
    }
}

async function handleFullDirectoryCopy(tool, config, overrideHomeDir = null, targetFolder = null) {
    let destDir;
    let displayPath;
    
    const shouldUseHome = !targetFolder || config.forceHomeInstall;
    if (!shouldUseHome) {
        // If targetFolder is specified, use it instead of home directory
        destDir = path.join(targetFolder, config.targetSubdir);
        displayPath = path.join(targetFolder, config.targetSubdir);
        showProgress(`Checking ${displayPath} directory`);
    } else {
        // Default behavior - use home directory
        const homeDir = overrideHomeDir || os.homedir();
        destDir = path.join(homeDir, config.targetSubdir);
        displayPath = `~/${config.targetSubdir}`;
        showProgress(`Checking ${displayPath} directory`);
    }
    
    if (!fs.existsSync(destDir)) {
        fs.mkdirSync(destDir, { recursive: true });
        completeProgress(`Created ${displayPath} directory`);
    } else {
        completeProgress(`Found ${displayPath} directory`);
    }

    showProgress(`Copying ${config.ruleDir} contents`);
    const sourceDir = path.join(__dirname, config.ruleDir);
    const filesCopied = copyDirectoryRecursive(sourceDir, destDir, config.excludeFiles || [], config.templateSubstitutions || {});
    completeProgress(`Copied ${filesCopied} files to ${displayPath}`);

    if (targetFolder && config.projectRootCopies) {
        if (!fs.existsSync(targetFolder)) {
            fs.mkdirSync(targetFolder, { recursive: true });
        }
        showProgress('Copying project root files');
        let rootFilesCopied = 0;
        for (const fileName of config.projectRootCopies) {
            const sourcePath = path.join(__dirname, config.ruleDir, fileName);
            const destPath = path.join(targetFolder, fileName);
            if (!fs.existsSync(sourcePath)) {
                continue;
            }

            const substitutions = (config.templateSubstitutions || {})[fileName] ||
                (fileName.endsWith('.md') ? (config.templateSubstitutions || {})['**/*.md'] : null);

            if (substitutions) {
                let content = fs.readFileSync(sourcePath, 'utf8');
                content = substituteTemplate(content, substitutions);
                fs.writeFileSync(destPath, content);
            } else {
                fs.copyFileSync(sourcePath, destPath);
            }
            rootFilesCopied++;
        }
        completeProgress(`Copied ${rootFilesCopied} project root files`);
    }

    // Copy skills folder for claude-code-4.5
    if (tool === 'claude-code-4.5') {
        showProgress('Copying skills folder');
        const skillsSource = path.join(__dirname, config.ruleDir, 'skills');
        const skillsDest = path.join(destDir, 'skills');

        if (fs.existsSync(skillsSource)) {
            const skillsFiles = copyDirectoryRecursive(skillsSource, skillsDest, [], config.templateSubstitutions || {});
            completeProgress(`Copied ${skillsFiles} skill files`);
        }

        // Smart compilation for browser-tools in webapp-testing skill
        const browserToolsDir = path.join(skillsDest, 'webapp-testing', 'bin');
        const browserToolsTs = path.join(browserToolsDir, 'browser-tools.ts');
        const browserToolsBinary = path.join(browserToolsDir, 'browser-tools');
        const rebuildFlag = process.argv.includes('--rebuild');

        if (fs.existsSync(browserToolsTs)) {
            // For claude-code-4.5: compile the binary
            if (tool === 'claude-code-4.5') {
                let shouldCompile = rebuildFlag;

                if (!fs.existsSync(browserToolsBinary)) {
                    shouldCompile = true;
                    showProgress('browser-tools binary missing, compiling');
                } else if (!rebuildFlag) {
                    const tsStats = statSync(browserToolsTs);
                    const binaryStats = statSync(browserToolsBinary);
                    if (tsStats.mtime > binaryStats.mtime) {
                        shouldCompile = true;
                        showProgress('browser-tools.ts is newer, recompiling');
                    }
                }

                if (shouldCompile) {
                    try {
                        showProgress('Compiling browser-tools binary');
                        const { execSync } = await import('child_process');

                        // Try Bun first
                        try {
                            execSync('which bun', { stdio: 'pipe' });
                            execSync(
                                `cd "${browserToolsDir}" && bun install && bun build browser-tools.ts --compile --target bun --outfile browser-tools`,
                                { stdio: 'pipe' }
                            );
                            completeProgress('Compiled browser-tools with Bun');
                        } catch {
                            // Fallback to esbuild
                            try {
                                execSync('which esbuild', { stdio: 'pipe' });
                                execSync(
                                    `cd "${browserToolsDir}" && npm install && esbuild browser-tools.ts --bundle --platform=node --outfile=browser-tools.js`,
                                    { stdio: 'pipe' }
                                );
                                completeProgress('Compiled browser-tools with esbuild (JavaScript output)');
                            } catch {
                                completeProgress('Compilation failed, using existing binary if available');
                            }
                        }
                    } catch (error) {
                        completeProgress(`Compilation error: ${error.message}, using existing binary if available`);
                    }
                }
            }
        }
    }

    // Copy tool-specific files if they exist
    if (config.toolSpecificFiles) {
        showProgress('Copying tool-specific files');
        let toolFilesCopied = 0;
        for (const toolFile of config.toolSpecificFiles) {
            const sourcePath = path.join(__dirname, toolFile);
            const fileName = path.basename(toolFile);
            const destPath = path.join(destDir, fileName);
            
            if (fs.existsSync(sourcePath)) {
                fs.copyFileSync(sourcePath, destPath);
                toolFilesCopied++;
            }
        }
        completeProgress(`Copied ${toolFilesCopied} tool-specific files`);
    }

    console.log(`\n\x1b[32m🎉 ${tool} setup complete!\x1b[0m`);
    console.log(`Files copied to: ${destDir}`);
}

// Discover available ClawdHub skills for interactive selection
function discoverClawdhubSkills(clawdhubDir) {
    const skills = [];

    if (!fs.existsSync(clawdhubDir)) {
        return skills;
    }

    const skillDirs = readdirSync(clawdhubDir).filter(f =>
        statSync(path.join(clawdhubDir, f)).isDirectory()
    );

    for (const skill of skillDirs) {
        const skillPath = path.join(clawdhubDir, skill);
        const skillMd = path.join(skillPath, 'SKILL.md');
        const skillJson = path.join(skillPath, 'skill.json');

        let description = skill;
        let version = 'unknown';

        // Try to get description from skill.json first
        if (fs.existsSync(skillJson)) {
            try {
                const json = JSON.parse(fs.readFileSync(skillJson, 'utf8'));
                if (json.description) description = json.description.split('.')[0]; // First sentence
                if (json.version) version = json.version;
            } catch {}
        }
        // Fallback to SKILL.md frontmatter
        else if (fs.existsSync(skillMd)) {
            const content = fs.readFileSync(skillMd, 'utf8');
            const descMatch = content.match(/description:\s*([^\n]+)/);
            if (descMatch) description = descMatch[1].trim().split('.')[0];
            const verMatch = content.match(/version:\s*["']?([^"'\n]+)/);
            if (verMatch) version = verMatch[1].trim();
        }

        skills.push({ name: skill, path: skill, description, version });
    }

    return skills;
}

async function handleClawdhubSkillsCopy(tool, config, targetFolder = null, isNonInteractive = false, specifiedSkills = null) {
    // Require target folder for ClawdHub skills
    if (!targetFolder) {
        const answers = await inquirer.prompt([
            {
                type: 'input',
                name: 'targetFolder',
                message: 'Enter the target workspace folder:',
                validate: (input) => !!input.trim() || 'Folder name required',
            },
        ]);
        targetFolder = answers.targetFolder;
    }

    const clawdhubDir = path.join(__dirname, config.ruleDir);
    const destDir = path.join(targetFolder, config.targetSubdir);
    let totalFilesCopied = 0;

    // Discover available skills
    const availableSkills = discoverClawdhubSkills(clawdhubDir);

    if (availableSkills.length === 0) {
        console.log('\\n⚠️  No ClawdHub skills found in clawdhub-skills/ directory.\\n');
        return;
    }

    let selectedSkillPaths = null;

    // If skills specified via CLI, use those
    if (specifiedSkills) {
        selectedSkillPaths = new Set(specifiedSkills);
        console.log(`\\n📦 Installing specified skills: ${specifiedSkills.join(', ')}\\n`);
    }
    // Interactive skill selection
    else if (!isNonInteractive) {
        console.log('\\n🦞 Select ClawdHub skills to install:\\n');

        const choices = availableSkills.map(skill => ({
            name: `${skill.name} (v${skill.version}) - ${skill.description}`,
            value: skill.name,
            checked: true
        }));

        const { selectedSkills } = await inquirer.prompt([
            {
                type: 'checkbox',
                name: 'selectedSkills',
                message: 'Use space to toggle, enter to confirm:',
                choices: choices,
                pageSize: 15
            }
        ]);

        if (selectedSkills.length === 0) {
            console.log('\\n⚠️  No skills selected.\\n');
            return;
        }

        selectedSkillPaths = new Set(selectedSkills);
    }
    // Non-interactive: copy all skills
    else {
        selectedSkillPaths = new Set(availableSkills.map(s => s.name));
    }

    showProgress(`Checking ${destDir} directory`);
    if (!fs.existsSync(destDir)) {
        fs.mkdirSync(destDir, { recursive: true });
        completeProgress(`Created ${destDir} directory`);
    } else {
        completeProgress(`Found ${destDir} directory`);
    }

    // Copy each selected skill
    for (const skillName of selectedSkillPaths) {
        const sourceDir = path.join(clawdhubDir, skillName);
        const targetDir = path.join(destDir, skillName);

        if (!fs.existsSync(sourceDir)) {
            console.log(`\\x1b[33m⚠\\x1b[0m  Skill not found: ${skillName}`);
            continue;
        }

        showProgress(`Copying skill: ${skillName}`);
        fs.mkdirSync(targetDir, { recursive: true });
        const filesCopied = copyDirectoryRecursive(
            sourceDir,
            targetDir,
            config.excludeFiles || [],
            config.templateSubstitutions || {}
        );
        totalFilesCopied += filesCopied;
        completeProgress(`Copied ${filesCopied} files from ${skillName}`);
    }

    console.log(`\\n\\x1b[32m🦞 ClawdHub skills installed!\\x1b[0m`);
    console.log(`Skills copied to: ${destDir} (${totalFilesCopied} files)`);
    console.log(`\\nInstalled skills:`);
    for (const skillName of selectedSkillPaths) {
        console.log(`  - ${skillName}/`);
    }
    console.log(`\\nTo publish to ClawdHub, run: clawdhub publish ${destDir}/<skill-name>`);
}

async function main() {
    const args = process.argv.slice(2);
    const toolArg = args.find(arg => arg.startsWith('--tool='));
    const targetFolderArg = args.find(arg => arg.startsWith('--targetFolder='));
    const homeDirArg = args.find(arg => arg.startsWith('--homeDir='));
    const packagesArg = args.find(arg => arg.startsWith('--packages='));
    const selectPackagesFlag = args.includes('--selectPackages');
    const sddShortcut = args.includes('--sdd');
    // If --selectPackages is passed, we want interactive package selection
    const isNonInteractive = (!!toolArg || sddShortcut) && !selectPackagesFlag;

    let tool = toolArg ? toolArg.split('=')[1] : null;
    let targetFolder = targetFolderArg ? targetFolderArg.split('=')[1] : null;
    let overrideHomeDir = homeDirArg ? homeDirArg.split('=')[1] : null;

    // Parse --packages=skills/webapp-testing,agents/engineering,...
    let specifiedPackages = null;
    if (packagesArg) {
        specifiedPackages = packagesArg.split('=')[1].split(',').map(p => p.trim());
    }

    // Convenience: allow --sdd without specifying a tool
    if (!tool && sddShortcut) {
        tool = 'sdd';
    }

    if (!tool) {
        const answers = await inquirer.prompt([
            {
                type: 'list',
                name: 'tool',
                message: 'Select the tool:',
                choices: [...Object.keys(TOOL_CONFIG), 'sdd'],
            },
        ]);
        tool = answers.tool;
    }

    // Handle SDD-only installation mode (no rules), copying assets from spec-kit into a project
    if (tool === 'sdd') {
        // Single, simple path: clone repo then copy assets
        const repo = process.env.SPEC_KIT_REPO || 'https://github.com/github/spec-kit.git';
        const ref = process.env.SPEC_KIT_REF || '';
        // pick destination
        const dest = targetFolder || (await inquirer.prompt([{ type: 'input', name: 'sddDest', message: 'Project folder for Spec Kit (SDD) assets:', validate: (v) => !!v.trim() || 'Folder name required' }])).sddDest;
        const clonePath = await cloneSpecKit(repo, ref);
        await copySpecKitAssets(clonePath, dest);
        return;
    }

    const config = TOOL_CONFIG[tool];

    // Handle ClawdHub skills (copy to target workspace/skills/)
    if (config.useClawdhubStructure) {
        await handleClawdhubSkillsCopy(tool, config, targetFolder, isNonInteractive, specifiedPackages);
        return;
    }

    // Handle packages structure (new catalog-based structure)
    if (config.usePackagesStructure) {
        await handlePackagesStructureCopy(tool, config, overrideHomeDir, targetFolder, isNonInteractive, specifiedPackages);
        return;
    }

    if (config.copyEntireFolder) {
        await handleFullDirectoryCopy(tool, config, overrideHomeDir, targetFolder);
        return;
    }

    if (config.copySharedContent) {
        await handleSharedContentCopy(tool, config, targetFolder);
        return;
    }

    if (!targetFolder) {
        const answers = await inquirer.prompt([
            {
                type: 'input',
                name: 'targetFolder',
                message: 'Enter the target project folder:',
                validate: (input) => !!input.trim() || 'Folder name required',
            },
        ]);
        targetFolder = answers.targetFolder;
    }
    
    showProgress('Creating target directory');
    if (!fs.existsSync(targetFolder)) {
        fs.mkdirSync(targetFolder, { recursive: true });
        completeProgress(`Created folder: ${targetFolder}`);
    } else {
        completeProgress(`Using existing folder: ${targetFolder}`);
    }

    showProgress('Copying tool-specific rules');
    const destDir = path.join(targetFolder, config.targetSubdir);
    fs.mkdirSync(destDir, { recursive: true });
    
    const rulePath = path.join(__dirname, config.ruleDir, config.ruleGlob);
    fs.copyFileSync(rulePath, path.join(destDir, config.ruleGlob));
    
    for (const rule of ALWAYS_COPY_RULES) {
        fs.copyFileSync(path.join(GENERAL_RULES_DIR, rule), path.join(destDir, rule));
    }
    completeProgress('Copied core rules');

    const copiedFiles = [config.ruleGlob, ...ALWAYS_COPY_RULES];
    const generalRuleFiles = getGeneralRuleFiles();
    if (!isNonInteractive && generalRuleFiles.length > 0) {
        const { selectedGeneralRules } = await inquirer.prompt([
            {
                type: 'checkbox',
                name: 'selectedGeneralRules',
                message: 'Select additional general rules to copy:',
                choices: generalRuleFiles,
            },
        ]);
        
        if (selectedGeneralRules && selectedGeneralRules.length > 0) {
            showProgress('Copying additional rules');
            for (const ruleFile of selectedGeneralRules) {
                fs.copyFileSync(path.join(GENERAL_RULES_DIR, ruleFile), path.join(destDir, ruleFile));
                copiedFiles.push(ruleFile);
            }
            completeProgress(`Copied ${selectedGeneralRules.length} additional rules`);
        }
    }

    // Copy shared content to specific target if specified
    if (config.sharedContentTarget) {
        const targetDir = path.join(targetFolder, config.sharedContentTarget);
        showProgress(`Copying shared content to ${config.sharedContentTarget}`);
        const sharedSourceDir = path.join(__dirname, config.sharedContentDir);
        fs.mkdirSync(targetDir, { recursive: true });
        const excludeFiles = getEffectiveExcludeFiles(tool, config);
        const sharedFilesCopied = copyDirectoryRecursive(sharedSourceDir, targetDir, excludeFiles, config.templateSubstitutions || {});
        completeProgress(`Copied ${sharedFilesCopied} shared files to ${config.sharedContentTarget}`);
    }

    // Copy MCP file to specified target if specified
    if (config.mcpFile && config.mcpTarget) {
        showProgress('Copying MCP configuration');
        const sourcePath = path.join(__dirname, config.mcpFile);
        const destPath = path.join(targetFolder, config.mcpTarget);
        
        if (fs.existsSync(sourcePath)) {
            fs.mkdirSync(path.dirname(destPath), { recursive: true });
            fs.copyFileSync(sourcePath, destPath);
            completeProgress(`Copied MCP config to ${config.mcpTarget}`);
        }
    }

    // Perform special file copies
    if (config.specialCopies) {
        showProgress('Performing special file copies');
        let specialFilesCopied = 0;
        for (const copy of config.specialCopies) {
            const sourcePath = path.join(__dirname, copy.source);
            const destPath = path.join(targetFolder, copy.dest);
            const fileName = path.basename(copy.source);

            if (fs.existsSync(sourcePath)) {
                fs.mkdirSync(path.dirname(destPath), { recursive: true });

                if (config.templateSubstitutions && config.templateSubstitutions[fileName]) {
                    let content = fs.readFileSync(sourcePath, 'utf8');
                    content = substituteTemplate(content, config.templateSubstitutions[fileName]);
                    fs.writeFileSync(destPath, content);
                } else {
                    fs.copyFileSync(sourcePath, destPath);
                }
                specialFilesCopied++;
            }
        }
        completeProgress(`Copied ${specialFilesCopied} special files`);
    }

    

    showProgress('Generating rule registry');
    const registry = {};
    for (const file of copiedFiles) {
        const filePath = path.join(destDir, file);
        const front = parseFrontMatter(filePath);
        if (front) {
            registry[file.replace(/\..*$/, '')] = {
                path: path.join(config.targetSubdir, file),
                globs: Array.isArray(front.globs) ? front.globs : (front.globs ? [front.globs] : []),
                alwaysApply: !!front.alwaysApply
            };
        }
    }
    fs.writeFileSync(path.join(destDir, 'rule-registry.json'), JSON.stringify(registry, null, 4));
    completeProgress('Generated rule registry');

    console.log('\n\x1b[32m🎉 Setup complete!\x1b[0m');
    console.log(`Files copied to: ${destDir}`);
}

main().catch((err) => {
    console.error(err);
    process.exit(1);
});

// --- Spec Kit integration: clone-on-demand, then copy ---
import { execSync } from 'child_process';

async function cloneSpecKit(repoUrl, ref = '') {
    const base = fs.mkdtempSync(path.join(os.tmpdir(), 'spec-kit-'));
    const cloneDir = path.join(base, 'src');
    showProgress(`Cloning Spec Kit from ${repoUrl}`);
    execSync(`git clone --depth 1 ${repoUrl} ${cloneDir}`, { stdio: 'pipe' });
    if (ref && ref.trim()) {
        execSync(`git -C ${cloneDir} fetch origin ${ref} --depth 1`, { stdio: 'pipe' });
        execSync(`git -C ${cloneDir} checkout ${ref}`, { stdio: 'pipe' });
    }
    completeProgress('Cloned Spec Kit');
    return cloneDir;
}
async function copySpecKitAssets(specKitRoot, projectRoot) {
    const requiredFiles = {
        claudeCommands: [
            { source: '.claude/commands/specify.md', dest: '.claude/commands/sdd-specify.md' },
            { source: '.claude/commands/plan.md', dest: '.claude/commands/sdd-plan.md' },
            { source: '.claude/commands/tasks.md', dest: '.claude/commands/sdd-tasks.md' },
        ],
        scripts: [
            'scripts/create-new-feature.sh',
            'scripts/setup-plan.sh',
            'scripts/check-task-prerequisites.sh',
            'scripts/common.sh',
            'scripts/get-feature-paths.sh',
            'scripts/update-agent-context.sh',
        ],
        templates: [
            'templates/spec-template.md',
            'templates/plan-template.md',
            'templates/tasks-template.md',
            'templates/agent-file-template.md',
        ],
        memory: [
            'memory/constitution.md',
        ],
    };

    const exists = (p) => fs.existsSync(p);
    const src = (rel) => path.join(specKitRoot, rel);
    const dst = (rel) => path.join(projectRoot, rel);

    if (!exists(specKitRoot)) {
        throw new Error(`[SDD] Spec Kit path not found: ${specKitRoot}`);
    }

    console.log(`\n⠋ Installing Spec Kit (SDD) assets from: ${specKitRoot}`);

    // Ensure directories
    const ensureParent = (p) => fs.mkdirSync(path.dirname(p), { recursive: true });
    const copyFileIdempotent = (from, to, makeExecutable = false) => {
        ensureParent(to);
        if (exists(to)) {
            const a = fs.readFileSync(from);
            const b = fs.readFileSync(to);
            if (Buffer.compare(a, b) === 0) {
                return 'skipped';
            }
            fs.copyFileSync(to, `${to}.bak`);
        }
        fs.copyFileSync(from, to);
        if (makeExecutable) {
            try { fs.chmodSync(to, 0o755); } catch {}
        }
        return 'copied';
    };

    // Commands (with renaming)
    for (const cmd of requiredFiles.claudeCommands) {
        const sourcePath = cmd.source;
        const destPath = cmd.dest;
        const from = src(`.${path.sep}${sourcePath.split('/').slice(1).join(path.sep)}`); // map .claude/commands under spec-kit root
        const to = dst(destPath);
        if (!exists(from)) {
            // In spec-kit, commands live at .claude/commands
            const alt = src(sourcePath);
            copyFileIdempotent(exists(alt) ? alt : from, to);
        } else {
            copyFileIdempotent(from, to);
        }
    }

    // Scripts (executable)
    for (const rel of requiredFiles.scripts) {
        const from = src(rel);
        const to = dst(rel);
        copyFileIdempotent(from, to, true);
    }

    // Templates
    for (const rel of requiredFiles.templates) {
        const from = src(rel);
        const to = dst(rel);
        copyFileIdempotent(from, to);
    }

    // Memory
    for (const rel of requiredFiles.memory) {
        const from = src(rel);
        const to = dst(rel);
        copyFileIdempotent(from, to);
    }

    // Optional quickstart doc (generated)
    const quickstartPath = dst('docs/sdd-quickstart.md');
    ensureParent(quickstartPath);
    if (!exists(quickstartPath)) {
        const qs = `# Spec-Driven Development (SDD) Quickstart\n\n- Requires: Git, bash (macOS/Linux/WSL).\n- Commands live in \`.claude/commands\` for Claude Code.\n\nWorkflow:\n- /sdd-specify → creates branch + \`specs/###-.../spec.md\`\n- /sdd-plan → fills \`plan.md\` and generates research/data model/contracts/quickstart\n- /sdd-tasks → creates \`tasks.md\` from available docs\n\nRun manually (if needed):\n- \`bash scripts/create-new-feature.sh --json \"My feature\"\`\n- \`bash scripts/setup-plan.sh --json\` (must be on feature branch)\n- \`bash scripts/check-task-prerequisites.sh --json\`\n\nBranch rule: must match \`^[0-9]{3}-\` for /sdd-plan and /sdd-tasks.\n`;
        fs.writeFileSync(quickstartPath, qs);
    }

    console.log('\x1b[32m✓ Installed Spec Kit assets\x1b[0m');
}
