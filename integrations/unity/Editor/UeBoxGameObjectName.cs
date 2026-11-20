// Copy this file into a temporary Unity project's Assets/Editor folder.
// Coplay custom tool: prepare a narrowly scoped rename, then apply its one-time ticket.
using System;
using System.Security.Cryptography;
using MCPForUnity.Editor.Helpers;
using MCPForUnity.Editor.Tools;
using Newtonsoft.Json.Linq;
using UnityEditor;
using UnityEditor.SceneManagement;
using UnityEngine;
using UnityEngine.SceneManagement;

namespace UeBox.Editor.CustomTools
{
    [McpForUnityTool(
        "uebox_prepare_or_apply_gameobject_name",
        Description = "Prepare or apply one approved rename of the currently selected ordinary active-scene GameObject. No scene or asset is saved.")]
    public static class UeBoxGameObjectName
    {
        private const double TicketLifetimeSeconds = 30.0;
        private const int MaxNameLength = 256;

        private static PreparedRename s_prepared;

        public sealed class Parameters
        {
            [ToolParameter("Use prepare to preview the selected object's current identity and reserve a one-time ticket, or apply to consume that ticket.")]
            public string action { get; set; }

            [ToolParameter("The proposed new name. Required for prepare; fixed into the ticket and not accepted by apply.", Required = false)]
            public string newName { get; set; }

            [ToolParameter("The opaque one-time ticket returned by prepare. Required for apply.", Required = false)]
            public string ticket { get; set; }
        }

        private sealed class PreparedRename
        {
            public GameObject target;
            public int sceneHandle;
            public int instanceId;
            public string oldName;
            public string newName;
            public string ticket;
            public double expiresAt;
        }

        public static object HandleCommand(JObject @params)
        {
            if (@params == null)
                return new ErrorResponse("Parameters are required");

            Parameters parameters;
            try
            {
                parameters = @params.ToObject<Parameters>();
            }
            catch
            {
                return new ErrorResponse("Invalid parameters");
            }

            if (parameters == null)
                return new ErrorResponse("Invalid parameters");

            if (parameters.action == "prepare")
                return Prepare(parameters.newName);

            if (parameters.action == "apply")
                return Apply(parameters.ticket);

            return new ErrorResponse("action must be prepare or apply");
        }

        private static object Prepare(string newName)
        {
            // A new prepare invalidates any prior approval, including if validation fails.
            s_prepared = null;

            if (!IsValidName(newName))
                return new ErrorResponse("newName must be a non-empty name of at most 256 characters without NUL");

            GameObject target;
            Scene scene;
            string failure;
            if (!TryGetCurrentTarget(out target, out scene, out failure))
                return new ErrorResponse(failure);

            string ticket;
            try
            {
                ticket = CreateTicket();
            }
            catch
            {
                return new ErrorResponse("Could not create a one-time approval ticket");
            }

            s_prepared = new PreparedRename
            {
                target = target,
                sceneHandle = scene.handle,
                instanceId = target.GetInstanceID(),
                oldName = target.name,
                newName = newName,
                ticket = ticket,
                expiresAt = EditorApplication.timeSinceStartup + TicketLifetimeSeconds
            };

            // These three preview values are the complete target identity shown for approval.
            return new SuccessResponse("Rename prepared; approval expires in 30 seconds", new
            {
                preview = new
                {
                    sceneHandle = s_prepared.sceneHandle,
                    gameObjectInstanceId = s_prepared.instanceId,
                    oldName = s_prepared.oldName
                },
                proposedName = s_prepared.newName,
                ticket = s_prepared.ticket,
                expiresInSeconds = TicketLifetimeSeconds
            });
        }

        private static object Apply(string suppliedTicket)
        {
            PreparedRename prepared = s_prepared;
            // Every apply attempt consumes the in-memory ticket before checking or mutating.
            s_prepared = null;

            if (prepared == null || string.IsNullOrEmpty(suppliedTicket) || !FixedTimeEquals(prepared.ticket, suppliedTicket))
                return ApplyResult("Approval ticket is invalid or already consumed", new
                {
                    status = "notExecuted",
                    reason = "ticket_invalid"
                });

            if (EditorApplication.timeSinceStartup > prepared.expiresAt)
                return ApplyResult("Approval ticket expired; prepare again", new
                {
                    status = "notExecuted",
                    reason = "ticket_expired"
                });

            GameObject current;
            Scene scene;
            try
            {
                string failure;
                if (!TryGetCurrentTarget(out current, out scene, out failure))
                    return TargetChanged(failure);

                // Recheck all captured identity and content synchronously before the first write.
                if (scene.handle != prepared.sceneHandle ||
                    current.GetInstanceID() != prepared.instanceId ||
                    current != prepared.target ||
                    !string.Equals(current.name, prepared.oldName, StringComparison.Ordinal))
                    return TargetChanged("Selected target or its name changed; prepare again");
            }
            catch
            {
                return TargetChanged("Selected target could not be revalidated; prepare again");
            }

            try
            {
                Undo.RecordObject(current, "Rename GameObject");
                current.name = prepared.newName;

                if (PrefabUtility.IsPartOfPrefabInstance(current))
                    PrefabUtility.RecordPrefabInstancePropertyModifications(current);

                // Establish identity before interpreting the returned name as a mismatch.
                GameObject[] selectedAfterWrite = Selection.gameObjects;
                Scene readBackScene = current.scene;
                bool identityMatches = current != null &&
                    selectedAfterWrite != null && selectedAfterWrite.Length == 1 &&
                    selectedAfterWrite[0] == current && current.GetInstanceID() == prepared.instanceId &&
                    readBackScene.IsValid() && readBackScene.isLoaded &&
                    readBackScene.handle == prepared.sceneHandle &&
                    SceneManager.GetActiveScene().handle == prepared.sceneHandle;
                if (!identityMatches)
                    return ApplyResult("Rename outcome could not be confirmed after writing", new
                    {
                        status = "unknown"
                    });

                if (!string.Equals(current.name, prepared.newName, StringComparison.Ordinal))
                    return ApplyResult("The same target was read back with a different name", new
                    {
                        status = "mismatch",
                        sceneHandle = readBackScene.handle,
                        gameObjectInstanceId = current.GetInstanceID(),
                        actualName = current.name
                    });

                return ApplyResult("GameObject renamed in memory; scene was not saved", new
                {
                    status = "applied",
                    sceneHandle = readBackScene.handle,
                    gameObjectInstanceId = current.GetInstanceID(),
                    oldName = prepared.oldName,
                    newName = current.name,
                    saved = false
                });
            }
            catch
            {
                return ApplyResult("An exception occurred after the write started; the final state is unknown", new
                {
                    status = "unknown"
                });
            }
        }

        private static object TargetChanged(string message)
        {
            return ApplyResult(message, new
            {
                status = "notExecuted",
                reason = "target_changed"
            });
        }

        private static object ApplyResult(string message, object data)
        {
            return new SuccessResponse(message, data);
        }

        private static bool TryGetCurrentTarget(out GameObject target, out Scene scene, out string failure)
        {
            target = null;
            scene = SceneManager.GetActiveScene();
            failure = null;

            if (EditorApplication.isPlaying || EditorApplication.isPlayingOrWillChangePlaymode)
            {
                failure = "Play mode or a play-mode transition is active";
                return false;
            }

            if (PrefabStageUtility.GetCurrentPrefabStage() != null)
            {
                failure = "Prefab Stage is not an ordinary active scene";
                return false;
            }

            if (!scene.IsValid() || !scene.isLoaded)
            {
                failure = "There is no loaded active scene";
                return false;
            }

            GameObject[] selected = Selection.gameObjects;
            target = selected != null && selected.Length == 1 ? selected[0] : null;
            if (target == null || Selection.activeGameObject != target || EditorUtility.IsPersistent(target) ||
                !target.scene.IsValid() || !target.scene.isLoaded || target.scene.handle != scene.handle ||
                EditorSceneManager.IsPreviewScene(target.scene))
            {
                target = null;
                failure = "Select a non-asset GameObject in the ordinary active scene";
                return false;
            }

            return true;
        }

        private static bool IsValidName(string value)
        {
            return !string.IsNullOrWhiteSpace(value) &&
                   value.Length <= MaxNameLength &&
                   Array.TrueForAll(value.ToCharArray(), character => !char.IsControl(character));
        }

        private static string CreateTicket()
        {
            byte[] bytes = new byte[32];
            using (RandomNumberGenerator random = RandomNumberGenerator.Create())
                random.GetBytes(bytes);

            char[] result = new char[bytes.Length * 2];
            const string hex = "0123456789abcdef";
            for (int i = 0; i < bytes.Length; i++)
            {
                result[i * 2] = hex[bytes[i] >> 4];
                result[i * 2 + 1] = hex[bytes[i] & 0x0f];
            }
            Array.Clear(bytes, 0, bytes.Length);
            return new string(result);
        }

        private static bool FixedTimeEquals(string expected, string actual)
        {
            if (expected == null || actual == null || expected.Length != actual.Length)
                return false;

            int difference = 0;
            for (int i = 0; i < expected.Length; i++)
                difference |= expected[i] ^ actual[i];
            return difference == 0;
        }
    }
}
