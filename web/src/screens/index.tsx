import type { ComponentType } from 'preact';
import { routeNode, type Route } from '../ui/router';
import { Agents } from './Agents';
import { Devices } from './Devices';
import { Inbox } from './Inbox';
import { Issues } from './Issues';
import { Mailbox } from './Mailbox';
import { NodeDetail } from './NodeDetail';
import { Nodes } from './Nodes';
import { Settings } from './Settings';

export const SCREENS: Record<Route, ComponentType> = { agents: Agents, issues: Issues, inbox: Inbox, nodes: NodesRoute, mailbox: Mailbox, devices: Devices, settings: Settings };

/** `#/nodes` lists them, `#/nodes/<id>` shows one. */
function NodesRoute() {
  const id = routeNode.value;
  return id === null ? <Nodes /> : <NodeDetail key={id} id={id} />;
}
