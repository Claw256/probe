#!/usr/bin/env node

// This script starts the Probe web interface

import path from 'path';
import { fileURLToPath } from 'url';
import { spawn } from 'child_process';
import fs from 'fs';

// Get the directory name of the current module
const __dirname = path.dirname(fileURLToPath(import.meta.url));
const packageDir = path.resolve(__dirname, '..');
const webDir = path.resolve(__dirname, '../web');
const parentDir = path.resolve(__dirname, '../..');

// Try to find main.js in different locations
let mainJsPath;
let cwd;

if (fs.existsSync(path.join(webDir, 'main.js'))) {
	// First check in the web directory inside the package
	mainJsPath = path.join(webDir, 'main.js');
	cwd = webDir;
} else if (fs.existsSync(path.join(parentDir, 'main.js'))) {
	// Then check in the parent directory (for development)
	mainJsPath = path.join(parentDir, 'main.js');
	cwd = parentDir;
} else {
	// If not found, try to use the one from the package
	mainJsPath = path.join(packageDir, 'main.js');
	if (!fs.existsSync(mainJsPath)) {
		console.error('Error: main.js not found. Please make sure you have the Probe web interface files installed.');
		console.error('Looked in:', webDir, parentDir, packageDir);
		process.exit(1);
	}
	cwd = packageDir;
}

console.log('Starting Probe Web Interface from:', cwd);
console.log('Using main.js from:', mainJsPath);

// Start the web server
const server = spawn('node', [mainJsPath], {
	cwd: cwd,
	stdio: 'inherit',
	env: {
		...process.env,
		PROBE_WEB_INTERFACE: 'true'
	}
});

// Handle server process events
server.on('error', (err) => {
	console.error('Failed to start web server:', err);
	process.exit(1);
});

server.on('close', (code) => {
	console.log(`Web server process exited with code ${code}`);
	process.exit(code);
});

// Handle termination signals
process.on('SIGINT', () => {
	console.log('Received SIGINT. Shutting down web server...');
	server.kill('SIGINT');
});

process.on('SIGTERM', () => {
	console.log('Received SIGTERM. Shutting down web server...');
	server.kill('SIGTERM');
});