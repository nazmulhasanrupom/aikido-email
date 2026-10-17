import { zodResolver } from "@hookform/resolvers/zod";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { useState } from "react";
import { useForm } from "react-hook-form";
import { z } from "zod";
import { acceptInvite, refreshAuthState } from "@/api/auth";
import { api, unwrap } from "@/api/client";
import { ApiProblem, messageOf } from "@/api/errors";
import { AuthLayout } from "@/components/AuthLayout";
import { Button } from "@/components/Button";
import { Field } from "@/components/Field";
import { PasswordField } from "@/components/PasswordField";
import { StrengthMeter } from "@/components/StrengthMeter";
import { HOME } from "@/lib/next";

export const Route = createFileRoute("/invite/$token")({ component: InvitePage });

const schema = z.object({
  name: z.string().trim().min(1, "Type your name.").max(100, "Use up to 100 characters."),
  password: z
    .string()
    .min(12, "Use at least 12 characters.")
    .max(128, "Use at most 128 characters."),
});
type Values = z.infer<typeof schema>;

function InvitePage() {
  const { token } = Route.useParams();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [problem, setProblem] = useState<string>();
  const info = useQuery({
    queryKey: ["invite", token],
    queryFn: () => unwrap(api.GET("/v1/auth/invites/{token}", { params: { path: { token } } })),
    retry: false,
  });
  const {
    register,
    handleSubmit,
    setError,
    watch,
    formState: { errors, isSubmitting },
  } = useForm<Values>({ resolver: zodResolver(schema) });
  const [password, name] = watch(["password", "name"]);

  if (info.isPending) return <AuthLayout title="Join">{null}</AuthLayout>;
  if (info.isError) {
    return (
      <AuthLayout title="This invite does not work">
        <p className="text-muted">{messageOf(info.error)}</p>
      </AuthLayout>
    );
  }

  async function onSubmit(values: Values) {
    setProblem(undefined);
    try {
      await acceptInvite(token, values.name, values.password);
      await refreshAuthState(queryClient);
      await navigate({ href: HOME });
    } catch (e) {
      if (e instanceof ApiProblem && e.code.startsWith("password_"))
        setError("password", { message: messageOf(e) });
      else setProblem(messageOf(e));
    }
  }

  return (
    <AuthLayout
      title={`Join ${info.data.workspace_name}`}
      intro={`You are invited as ${info.data.role}: ${info.data.email}`}
    >
      <form onSubmit={handleSubmit(onSubmit)} noValidate className="flex flex-col gap-4">
        <Field
          label="Your name"
          autoComplete="name"
          autoFocus
          error={errors.name?.message}
          {...register("name")}
        />
        <div className="flex flex-col gap-2">
          <PasswordField
            label="Password"
            autoComplete="new-password"
            help="At least 12 characters."
            error={errors.password?.message}
            {...register("password")}
          />
          <StrengthMeter password={password ?? ""} inputs={[name ?? "", info.data.email]} />
        </div>
        {problem && (
          <p role="alert" className="text-sm text-danger">
            {problem}
          </p>
        )}
        <Button type="submit" busy={isSubmitting}>
          Create account
        </Button>
      </form>
    </AuthLayout>
  );
}
