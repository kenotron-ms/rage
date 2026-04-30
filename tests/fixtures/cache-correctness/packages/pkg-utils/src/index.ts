import { greet } from "@fix-cc/pkg-core";
export const formatGreeting = (name: string): string => greet(name).toUpperCase();
