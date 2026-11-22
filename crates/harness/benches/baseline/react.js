import React from "react.mjs";
import { render, renderer } from "renderer.mjs";
import * as console from "console";

let firstContainer;
const updateContainer = { children: [] };
let updateRoot;

function tree(id, text) {
    return React.createElement("scene", { id }, text);
}

export function first() {
    firstContainer = { children: [] };
    render(tree("first", "initial content"), firstContainer);
}

export function mountUpdates() {
    updateRoot = renderer.createContainer(updateContainer, 0, null, false, null, "headless", console.error, null);
    renderer.updateContainer(tree("first", "initial content"), updateRoot, null, null);
}

export function hasUpdateRoot() {
    return updateRoot === undefined ? 0 : 1;
}

export function update(index) {
    renderer.updateContainer(tree("update-" + index, "content " + index), updateRoot, null, null);
}

function matches(container, id, text) {
    if (container.children.length !== 1) return 0;
    const scene = container.children[0];
    return scene.type === "scene" && scene.props.id === id && scene.children.length === 1 &&
        scene.children[0].text === text ? 1 : 0;
}

export function checkFirst() {
    return matches(firstContainer, "first", "initial content");
}

export function checkUpdateMount() {
    return matches(updateContainer, "first", "initial content");
}

export function checkUpdate(index) {
    return matches(updateContainer, "update-" + index, "content " + index);
}
