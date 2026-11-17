import type { ComponentType } from 'preact';
import type { Route } from '../ui/router';
import { Agents } from './Agents';
import { Devices } from './Devices';
import { Inbox } from './Inbox';
import { Mailbox } from './Mailbox';
import { Nodes } from './Nodes';
import { Settings } from './Settings';

export const SCREENS: Record<Route, ComponentType> = { agents: Agents, inbox: Inbox, nodes: Nodes, mailbox: Mailbox, devices: Devices, settings: Settings };
