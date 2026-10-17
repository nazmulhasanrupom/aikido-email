import * as Menu from "@radix-ui/react-dropdown-menu";
import type { ComponentProps } from "react";
import { cn } from "@/lib/cn";

export const MenuRoot = Menu.Root;
export const MenuTrigger = Menu.Trigger;
export const MenuRadioGroup = Menu.RadioGroup;
export const MenuSeparator = (props: ComponentProps<typeof Menu.Separator>) => (
  <Menu.Separator className="my-1 h-px bg-border" {...props} />
);
export const MenuLabel = ({ className, ...rest }: ComponentProps<typeof Menu.Label>) => (
  <Menu.Label className={cn("px-2 py-1.5 text-xs font-medium text-muted", className)} {...rest} />
);

export function MenuContent({ className, ...rest }: ComponentProps<typeof Menu.Content>) {
  return (
    <Menu.Portal>
      <Menu.Content
        sideOffset={6}
        className={cn(
          "animate-fade-in z-50 min-w-52 rounded-(--radius-card) border border-border bg-surface p-1 shadow-sm",
          className,
        )}
        {...rest}
      />
    </Menu.Portal>
  );
}

const ITEM =
  "flex cursor-default select-none items-center gap-2 rounded-(--radius-control) px-2 py-1.5 text-sm outline-none data-[disabled]:opacity-60 data-[highlighted]:bg-surface-2";

export function MenuItem({ className, ...rest }: ComponentProps<typeof Menu.Item>) {
  return <Menu.Item className={cn(ITEM, className)} {...rest} />;
}

export function MenuRadioItem({
  className,
  children,
  ...rest
}: ComponentProps<typeof Menu.RadioItem>) {
  return (
    <Menu.RadioItem className={cn(ITEM, "justify-between", className)} {...rest}>
      {children}
      <Menu.ItemIndicator className="text-accent">&#10003;</Menu.ItemIndicator>
    </Menu.RadioItem>
  );
}
