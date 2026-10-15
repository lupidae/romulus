import { useNavigate } from "@tanstack/react-router";
import * as React from "react";

import { ApiError } from "@/api/client";
import { InfoCircleIcon, LogOutIcon, MoreHorizIcon, TrashIcon } from "@/components/icons";
import { Avatar } from "@/components/ui/avatar";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { ConfirmDialog } from "@/components/ui/confirm-dialog";
import { Menu, MenuContent, MenuItem, MenuLabel, MenuTrigger } from "@/components/ui/dropdown-menu";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { toast } from "@/components/ui/toaster";
import { Tooltip } from "@/components/ui/tooltip";
import { formatDate } from "@/lib/time";

import { useChangeRole, useLeaveOrg, useRemoveMember } from "../mutations";
import type { Org, OrgMember, Role } from "../queries";
import { canManageMembers, roleLabel, withArticle } from "../roles";
import { RoleSelect } from "./role-select";

type Pending =
  | { kind: "remove"; member: OrgMember }
  | { kind: "leave"; member: OrgMember }
  | { kind: "demote-self"; member: OrgMember; role: Role };

const firstName = (name: string) => name.trim().split(/\s+/)[0] ?? name;

function failed(error: unknown) {
  toast.error(error instanceof ApiError ? error.message : "Something went wrong. Try again.");
}

interface MembersTableProps {
  org: Org;
  members: OrgMember[];
  myId: string;
}

/**
 * Everyone in the organization. Admins change roles in place and remove
 * people; everyone can leave. The last admin is explained, not just greyed
 * out: the role list and the menu both say why it cannot change yet.
 */
export function MembersTable({ org, members, myId }: MembersTableProps) {
  const navigate = useNavigate();
  const admin = canManageMembers(org.role);
  const changeRole = useChangeRole(org.slug, myId);
  const remove = useRemoveMember(org.slug);
  const leave = useLeaveOrg(org.slug, myId);
  const [pending, setPending] = React.useState<Pending>();

  const admins = members.filter((m) => m.role === "admin").length;
  const lastAdmin = (m: OrgMember) => m.role === "admin" && admins === 1;
  const lockedReason = `${org.name} needs at least one admin. Make someone else an admin first.`;

  const applyRole = (member: OrgMember, role: Role) => {
    const mine = member.user_id === myId;
    return changeRole.mutateAsync(
      { userId: member.user_id, role },
      {
        onSuccess: () =>
          toast.success(
            mine
              ? `You are now ${withArticle(role)}`
              : `${firstName(member.name)} is now ${withArticle(role)}`,
          ),
        onError: failed,
      },
    );
  };

  const pickRole = (member: OrgMember, role: Role) => {
    if (role === member.role) return;
    // Stepping down is the one change you cannot undo yourself: ask first.
    if (member.user_id === myId && member.role === "admin") {
      setPending({ kind: "demote-self", member, role });
      return;
    }
    // A failure is reported by onError and rolled back by the mutation.
    applyRole(member, role).catch(() => undefined);
  };

  const confirm = async () => {
    if (!pending) return;
    const { member } = pending;
    if (pending.kind === "demote-self") {
      await applyRole(member, pending.role);
      return;
    }
    try {
      if (pending.kind === "leave") await leave.mutateAsync();
      else await remove.mutateAsync(member.user_id);
    } catch (error) {
      failed(error);
      throw error;
    }
    if (pending.kind === "leave") {
      toast.success(`You left ${org.name}`);
      await navigate({ to: "/" });
    } else {
      toast.success(`${member.name} was removed from ${org.name}`);
    }
  };

  return (
    <>
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Member</TableHead>
            <TableHead className="hidden sm:table-cell">Role</TableHead>
            <TableHead className="hidden sm:table-cell">Joined</TableHead>
            <TableHead className="w-12">
              <span className="sr-only">Actions</span>
            </TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {members.map((member) => {
            const me = member.user_id === myId;
            const locked = lastAdmin(member);
            const role = admin ? (
              <RoleSelect
                value={member.role}
                onValueChange={(next) => pickRole(member, next)}
                lockedReason={locked ? lockedReason : undefined}
                aria-label={`Role of ${member.name}`}
                className="w-32"
              />
            ) : (
              <Badge variant={member.role === "admin" ? "primary" : "neutral"}>
                {roleLabel[member.role]}
              </Badge>
            );
            return (
              <TableRow key={member.user_id}>
                <TableCell className="w-full max-w-0 py-2.5">
                  <div className="flex min-w-0 items-center gap-3">
                    <Avatar id={member.user_id} name={member.name} size="lg" />
                    <div className="grid min-w-0">
                      <span className="flex min-w-0 items-center gap-1.5">
                        <span className="truncate font-medium text-foreground">{member.name}</span>
                        {me && <Badge variant="neutral">You</Badge>}
                      </span>
                      <span className="truncate text-muted-foreground">{member.email}</span>
                    </div>
                  </div>
                  {/* Phones: the role under the person, so names keep the width. */}
                  <div className="mt-2 pl-11 sm:hidden">{role}</div>
                </TableCell>
                <TableCell className="hidden sm:table-cell">{role}</TableCell>
                <TableCell className="hidden whitespace-nowrap text-muted-foreground tabular sm:table-cell">
                  {formatDate(member.joined_at)}
                </TableCell>
                <TableCell className="pr-2 align-top sm:align-middle">
                  {(admin || me) && (
                    <RowMenu
                      member={member}
                      me={me}
                      locked={locked}
                      orgName={org.name}
                      onRemove={() => setPending({ kind: me ? "leave" : "remove", member })}
                    />
                  )}
                </TableCell>
              </TableRow>
            );
          })}
        </TableBody>
      </Table>

      <ConfirmDialog
        open={pending !== undefined}
        onOpenChange={(open) => !open && setPending(undefined)}
        destructive={pending?.kind !== "demote-self"}
        onConfirm={confirm}
        {...(pending?.kind === "demote-self"
          ? {
              title: "Give up admin access?",
              description: `You will be ${withArticle(pending.role)} in ${org.name}. Only another admin can make you an admin again.`,
              confirmLabel: `Become ${withArticle(pending.role)}`,
            }
          : pending?.kind === "leave"
            ? {
                title: `Leave ${org.name}?`,
                description: `You lose access to its databases and drafts right away. To come back, an admin has to invite you again.`,
                confirmLabel: "Leave organization",
              }
            : {
                title: `Remove ${pending?.member.name ?? ""} from ${org.name}?`,
                description: `They lose access to its databases right away, and drafts they have open close for them. You can invite them again later.`,
                confirmLabel: "Remove member",
              })}
      />
    </>
  );
}

function RowMenu({
  member,
  me,
  locked,
  orgName,
  onRemove,
}: {
  member: OrgMember;
  me: boolean;
  locked: boolean;
  orgName: string;
  onRemove: () => void;
}) {
  const label = me ? `Leave ${orgName}…` : `Remove from ${orgName}…`;
  return (
    <Menu>
      <Tooltip content="More actions">
        <MenuTrigger asChild>
          <Button variant="ghost" size="icon" aria-label={`More actions for ${member.name}`}>
            <MoreHorizIcon />
          </Button>
        </MenuTrigger>
      </Tooltip>
      <MenuContent align="end" className="w-64">
        <MenuItem variant="destructive" disabled={locked} onSelect={onRemove}>
          {me ? <LogOutIcon /> : <TrashIcon />}
          {label}
        </MenuItem>
        {locked && (
          <MenuLabel className="flex items-start gap-1.5 font-normal">
            <InfoCircleIcon className="mt-0.5 size-3.5 shrink-0" />
            {me ? "You are the only admin. " : "This is the only admin. "}
            Make someone else an admin first.
          </MenuLabel>
        )}
      </MenuContent>
    </Menu>
  );
}

/** Rows with the real shape while members load. */
export function MembersTableSkeleton() {
  return (
    <Table aria-busy>
      <TableHeader>
        <TableRow>
          <TableHead>Member</TableHead>
          <TableHead className="hidden sm:table-cell">Role</TableHead>
          <TableHead className="hidden sm:table-cell">Joined</TableHead>
          <TableHead className="w-12">
            <span className="sr-only">Actions</span>
          </TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {[0, 1, 2].map((i) => (
          <TableRow key={i}>
            <TableCell className="w-full max-w-0">
              <div className="flex items-center gap-3">
                <Skeleton className="size-8 rounded-full" />
                <div className="grid flex-1 gap-1.5">
                  <Skeleton className="h-3.5 w-32" />
                  <Skeleton className="h-3.5 w-48 max-w-full" />
                </div>
              </div>
            </TableCell>
            <TableCell className="hidden sm:table-cell">
              <Skeleton className="h-8 w-32" />
            </TableCell>
            <TableCell className="hidden sm:table-cell">
              <Skeleton className="h-3.5 w-14" />
            </TableCell>
            <TableCell />
          </TableRow>
        ))}
      </TableBody>
    </Table>
  );
}
