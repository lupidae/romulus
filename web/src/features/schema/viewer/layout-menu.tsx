import { ViewColumns2Icon } from "@/components/icons";
import { Button } from "@/components/ui/button";
import {
  Menu,
  MenuCheckboxItem,
  MenuContent,
  MenuLabel,
  MenuRadioGroup,
  MenuRadioItem,
  MenuSeparator,
  MenuShortcut,
  MenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Tooltip } from "@/components/ui/tooltip";

export type HiddenPane = "none" | "graph" | "text";

interface LayoutMenuProps {
  hidden: HiddenPane;
  onHiddenChange: (hidden: HiddenPane) => void;
  fullScreen: boolean;
  onFullScreenChange: (fullScreen: boolean) => void;
}

/** Which panes show, and full screen: rare choices, one button for all of them. */
export function LayoutMenu({
  hidden,
  onHiddenChange,
  fullScreen,
  onFullScreenChange,
}: LayoutMenuProps) {
  return (
    <Menu>
      <Tooltip content="Layout">
        <MenuTrigger asChild>
          <Button variant="ghost" size="icon-sm" aria-label="Layout">
            <ViewColumns2Icon />
          </Button>
        </MenuTrigger>
      </Tooltip>
      <MenuContent align="end">
        <MenuLabel>Show</MenuLabel>
        <MenuRadioGroup value={hidden} onValueChange={(v) => onHiddenChange(v as HiddenPane)}>
          <MenuRadioItem value="none">Graph and text</MenuRadioItem>
          <MenuRadioItem value="text">Graph only</MenuRadioItem>
          <MenuRadioItem value="graph">Text only</MenuRadioItem>
        </MenuRadioGroup>
        <MenuSeparator />
        <MenuCheckboxItem
          checked={fullScreen}
          onCheckedChange={(v) => onFullScreenChange(v === true)}
        >
          Full screen
          {fullScreen && <MenuShortcut>Esc</MenuShortcut>}
        </MenuCheckboxItem>
      </MenuContent>
    </Menu>
  );
}
