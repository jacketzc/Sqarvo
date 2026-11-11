namespace Sqarvo.Core.Tests.Support;

/// <summary>
/// Records progress synchronously. <see cref="Progress{T}"/> posts to the captured synchronization
/// context, which makes assertions racy under xunit.
/// </summary>
internal sealed class RecordingProgress<T> : IProgress<T>
{
    private readonly List<T> _values = new();

    internal IReadOnlyList<T> Values
    {
        get
        {
            lock (_values)
            {
                return _values.ToArray();
            }
        }
    }

    public void Report(T value)
    {
        lock (_values)
        {
            _values.Add(value);
        }
    }
}